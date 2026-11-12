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

fn insert_employment_event_tx(
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

fn insert_hire_request_tx(tx: &Transaction<'_>, request: &AgentHireRequest) -> Result<()> {
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

fn validate_hire_preflight_tx(tx: &Transaction<'_>, request: &AgentHireRequest) -> Result<()> {
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

fn build_hire_request(
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

/// Create one explicit hire request. When approval is not required (or the
/// caller supplies explicit `approved: true`) this creates the identity in the
/// same transaction; otherwise it persists only a pending request and emits
/// `agent.hire.requested`. It never creates a session, assignment, workspace,
/// inbox, or memory row.
pub fn hire_agent(
    store: &Store,
    params: &vp_protocol::types::AgentHireParams,
) -> Result<AgentHireResult> {
    let project_id = params.project_id.trim().to_string();
    let operator = normalize_operator(params.operator.as_deref())?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    project_exists_tx(&tx, &project_id)?;
    let policy = agent_identity_policy_tx(&tx, &project_id)?;
    let now = super::now_rfc3339();
    let mut request = build_hire_request(params, Uuid::now_v7().to_string(), &operator, &now)?;
    validate_hire_preflight_tx(&tx, &request)?;
    let requires_approval = policy.hire_requires_approval && !params.approved;
    if requires_approval {
        insert_hire_request_tx(&tx, &request)?;
        insert_employment_event_tx(
            &tx,
            &project_id,
            &request.identity_id,
            AGENT_HIRE_REQUESTED_EVENT,
            serde_json::json!({
                "request": request,
                "approval_required": true,
                "approval_risk": request.approval_risk,
            }),
        )?;
        tx.commit()?;
        return Ok(AgentHireResult {
            request,
            identity: None,
            state: "pending".to_string(),
            changed: true,
        });
    }

    let pending = request.clone();
    request.status = "approved".to_string();
    request.decided_at = Some(now.clone());
    request.decided_by = Some(operator.clone());
    let create = create_params_from_hire(&request);
    let identity = create_agent_identity_with_id_tx(&tx, &create, Some(&request.identity_id))?;
    let identity_event = insert_agent_identity_event_tx(
        &tx,
        &identity,
        AGENT_IDENTITY_CREATED_EVENT,
        serde_json::json!({
            "identity": identity,
            "changed_fields": [],
            "previous_generation": 0,
            "generation": identity.generation,
            "hire_request_id": request.id,
            "approved_by": operator,
            "approval_risk": request.approval_risk,
        }),
    )?;
    let _ = identity_event;
    insert_hire_request_tx(&tx, &request)?;
    insert_employment_event_tx(
        &tx,
        &project_id,
        &identity.id,
        AGENT_HIRE_REQUESTED_EVENT,
        serde_json::json!({
            "request": pending,
            "approval_required": false,
            "approval_risk": request.approval_risk,
        }),
    )?;
    let approved_event = insert_employment_event_tx(
        &tx,
        &project_id,
        &identity.id,
        AGENT_HIRE_APPROVED_EVENT,
        serde_json::json!({
            "request": request,
            "identity": identity,
            "approved_by": operator,
            "approval_risk": request.approval_risk,
        }),
    )?;
    let _ = approved_event;
    tx.commit()?;
    Ok(AgentHireResult {
        request,
        identity: Some(identity),
        state: "approved".to_string(),
        changed: true,
    })
}

pub fn approve_agent_hire(
    store: &Store,
    params: &vp_protocol::types::AgentHireApproveParams,
) -> Result<AgentHireResult> {
    let operator = normalize_operator(params.operator.as_deref())?;
    let approval_risk = normalize_approval_risk(params.approval_risk.as_deref())?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut request = get_hire_request_tx(&tx, params.request_id.trim())?.ok_or_else(|| {
        employment_error(
            agent_identity_codes::HIRE_NOT_FOUND,
            format!("hire request not found: {}", params.request_id.trim()),
        )
    })?;
    if request.origin == "imported" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported hire request is archive evidence and cannot be approved locally",
        ));
    }
    if request.status != "pending" {
        return Err(employment_error(
            agent_identity_codes::HIRE_ALREADY_DECIDED,
            format!("hire request {} is already {}", request.id, request.status),
        ));
    }
    validate_hire_preflight_tx(&tx, &request)?;
    let now = super::now_rfc3339();
    request.status = "approved".to_string();
    request.decided_at = Some(now);
    request.decided_by = Some(operator.clone());
    request.approval_risk = approval_risk;
    let create = create_params_from_hire(&request);
    let identity = create_agent_identity_with_id_tx(&tx, &create, Some(&request.identity_id))?;
    insert_agent_identity_event_tx(
        &tx,
        &identity,
        AGENT_IDENTITY_CREATED_EVENT,
        serde_json::json!({
            "identity": identity,
            "changed_fields": [],
            "previous_generation": 0,
            "generation": identity.generation,
            "hire_request_id": request.id,
            "approved_by": operator,
            "approval_risk": request.approval_risk,
        }),
    )?;
    update_hire_request_tx(&tx, &request)?;
    insert_employment_event_tx(
        &tx,
        &request.project_id,
        &identity.id,
        AGENT_HIRE_APPROVED_EVENT,
        serde_json::json!({
            "request": request,
            "identity": identity,
            "approved_by": operator,
            "approval_risk": request.approval_risk,
        }),
    )?;
    tx.commit()?;
    Ok(AgentHireResult {
        request,
        identity: Some(identity),
        state: "approved".to_string(),
        changed: true,
    })
}

pub fn reject_agent_hire(
    store: &Store,
    params: &vp_protocol::types::AgentHireRejectParams,
) -> Result<AgentHireResult> {
    let operator = normalize_operator(params.operator.as_deref())?;
    let reason = normalize_reason(params.reason.as_deref(), "reason")?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut request = get_hire_request_tx(&tx, params.request_id.trim())?.ok_or_else(|| {
        employment_error(
            agent_identity_codes::HIRE_NOT_FOUND,
            format!("hire request not found: {}", params.request_id.trim()),
        )
    })?;
    if request.origin == "imported" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported hire request is archive evidence and cannot be rejected locally",
        ));
    }
    if request.status != "pending" {
        return Err(employment_error(
            agent_identity_codes::HIRE_ALREADY_DECIDED,
            format!("hire request {} is already {}", request.id, request.status),
        ));
    }
    request.status = "rejected".to_string();
    request.decided_at = Some(super::now_rfc3339());
    request.decided_by = Some(operator.clone());
    request.reason = reason.clone();
    update_hire_request_tx(&tx, &request)?;
    insert_employment_event_tx(
        &tx,
        &request.project_id,
        &request.identity_id,
        AGENT_HIRE_REJECTED_EVENT,
        serde_json::json!({
            "request": request,
            "rejected_by": operator,
            "reason": reason,
        }),
    )?;
    tx.commit()?;
    Ok(AgentHireResult {
        request,
        identity: None,
        state: "rejected".to_string(),
        changed: true,
    })
}

pub fn list_agent_hire_requests(
    store: &Store,
    params: &AgentHireListParams,
) -> Result<AgentHireListResult> {
    let limit = params
        .limit
        .unwrap_or(100)
        .clamp(1, AGENT_EMPLOYMENT_MAX_LIST) as i64;
    if let Some(status) = &params.status
        && !AGENT_HIRE_REQUEST_STATUSES.contains(&status.as_str())
    {
        return Err(employment_error(
            agent_identity_codes::HIRE_INVALID,
            format!("status must be pending, approved, or rejected: {status}"),
        ));
    }
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "{HIRE_REQUEST_SELECT}
         WHERE project_id = ?1 AND (?2 IS NULL OR status = ?2)
         ORDER BY requested_at DESC, id DESC LIMIT ?3"
    ))?;
    let requests = statement
        .query_map(
            params![params.project_id.trim(), params.status.as_deref(), limit],
            map_hire_request,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentHireListResult { requests })
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

/// Promote a Worker to Manager or a Manager to Leader. This is explicit and
/// approval-classified; it is never inferred from a plan or a model decision.
pub fn promote_agent(store: &Store, params: &AgentPromoteParams) -> Result<AgentPromoteResult> {
    if !params.approved {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED,
            "promotion requires explicit approved: true; this operation is never automatic",
        ));
    }
    let operator = normalize_operator(params.operator.as_deref())?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_agent_identity_tx(&tx, params.identity_id.trim())?;
    if existing.origin != "local" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported identity is read-only until an explicit rehome",
        ));
    }
    if existing.status == "retired" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_RETIRED,
            format!("identity {} is retired and terminal", existing.id),
        ));
    }
    let target_role = params.target_role.trim().to_ascii_lowercase();
    if !AGENT_PROMOTION_TARGETS.contains(&target_role.as_str()) {
        return Err(employment_error(
            agent_identity_codes::PROMOTION_INVALID,
            "target_role must be manager or leader",
        ));
    }
    let legal = matches!(
        (existing.role.as_str(), target_role.as_str()),
        ("worker", "manager") | ("manager", "leader")
    );
    if !legal {
        return Err(employment_error(
            agent_identity_codes::PROMOTION_INVALID,
            format!(
                "identity {} cannot be promoted from {} to {target_role}",
                existing.id, existing.role
            ),
        ));
    }
    ensure_identity_role_capacity_tx(&tx, &existing.project_id, &target_role, Some(&existing.id))?;

    let parent = if target_role == "leader" {
        if params
            .parent_identity_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(employment_error(
                agent_identity_codes::PROMOTION_INVALID,
                "a Leader identity cannot have a parent identity",
            ));
        }
        None
    } else if let Some(parent) = params
        .parent_identity_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(parent.to_string())
    } else {
        // A Worker's existing parent is a Manager. Promoting to Manager keeps
        // the structural rule Manager→Leader, so select the project's live
        // Leader deterministically when the caller omitted the parent.
        tx.query_row(
            "SELECT id FROM agent_identities
             WHERE project_id = ?1 AND role = 'leader' AND status <> 'retired'
             ORDER BY created_at ASC, id ASC LIMIT 1",
            params![existing.project_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    };
    validate_identity_parent_tx(&tx, &existing.project_id, &target_role, parent.as_deref())?;
    let title = match &params.title {
        Some(title) => normalize_identity_text(
            Some(title.as_str()),
            "title",
            vp_protocol::types::AGENT_IDENTITY_MAX_TITLE_CHARS,
        )?,
        None => existing.title.clone(),
    };

    let (active_assignments, active_workspaces, active_bindings) =
        identity_conflict_counts_tx(&tx, &existing.id)?;
    if (active_assignments > 0 || active_workspaces > 0 || active_bindings > 0) && !params.force {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_BUSY,
            format!(
                "identity {} still has {} active assignment(s), {} active workspace(s), and {} active workspace binding(s); release/archive them or pass force",
                existing.id, active_assignments, active_workspaces, active_bindings
            ),
        ));
    }
    let now = super::now_rfc3339();
    let mut released_assignments = 0;
    let mut archived_workspaces = 0;
    if params.force {
        let released = super::coordination::release_identity_assignments_tx(
            &tx,
            &existing.id,
            &operator,
            Some("promotion"),
            &now,
        )?;
        released_assignments = released.len() as u64;
        archived_workspaces =
            archive_identity_workspaces_tx(&tx, &existing.id, &now, &operator, Some("promotion"))?
                .len() as u64;
    }

    let mut next = existing.clone();
    next.role = target_role.clone();
    next.parent_identity_id = parent;
    next.title = title;
    next.generation = existing.generation.saturating_add(1);
    next.updated_at = now;
    let mut changed = vec!["role", "generation"];
    if next.parent_identity_id != existing.parent_identity_id {
        changed.push("parent_identity_id");
    }
    if next.title != existing.title {
        changed.push("title");
    }
    update_identity_and_event_tx(
        &tx,
        &existing,
        &next,
        &changed,
        vp_protocol::types::AGENT_IDENTITY_UPDATED_EVENT,
        serde_json::json!({
            "approved_by": operator,
            "approval_risk": "host_state",
        }),
    )?;
    let employment = insert_employment_event_tx(
        &tx,
        &existing.project_id,
        &existing.id,
        AGENT_PROMOTE_APPROVED_EVENT,
        serde_json::json!({
            "identity": next,
            "previous_role": existing.role,
            "previous_generation": existing.generation,
            "approved_by": operator,
            "approval_risk": "host_state",
            "released_assignments": released_assignments,
            "archived_workspaces": archived_workspaces,
        }),
    )?;
    tx.commit()?;
    Ok(AgentPromoteResult {
        identity: next,
        previous_role: existing.role,
        changed: true,
        released_assignments,
        archived_workspaces,
        employment_event_id: Some(employment.id),
    })
}

/// Transfer a non-Leader identity to a different department/component/title or
/// parent. A role change is promotion and is rejected here. Active work must be
/// released first unless `force` is explicit; even then, branches and evidence
/// are preserved.
pub fn transfer_agent(store: &Store, params: &AgentTransferParams) -> Result<AgentTransferResult> {
    if !params.approved {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED,
            "transfer requires explicit approved: true; this operation is never automatic",
        ));
    }
    if params.department.is_none()
        && params.component.is_none()
        && params.title.is_none()
        && params.parent_identity_id.is_none()
    {
        return Err(employment_error(
            agent_identity_codes::TRANSFER_INVALID,
            "transfer requires at least one of department, component, title, or parent_identity_id",
        ));
    }
    let operator = normalize_operator(params.operator.as_deref())?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_agent_identity_tx(&tx, params.identity_id.trim())?;
    if existing.origin != "local" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported identity is read-only until an explicit rehome",
        ));
    }
    if existing.status == "retired" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_RETIRED,
            format!("identity {} is retired and terminal", existing.id),
        ));
    }
    if existing.role == "leader"
        && params.component.as_ref().is_some_and(|value| {
            value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
    {
        return Err(employment_error(
            agent_identity_codes::TRANSFER_INVALID,
            "a Leader identity is project-wide and cannot own a component",
        ));
    }
    let mut next = existing.clone();
    let mut changed = Vec::new();
    if let Some(department) = &params.department {
        let value = normalize_identity_text(
            department.as_deref(),
            "department",
            vp_protocol::types::AGENT_IDENTITY_MAX_DEPARTMENT_CHARS,
        )?;
        if value != existing.department {
            next.department = value;
            changed.push("department");
        }
    }
    if let Some(component) = &params.component {
        let value = normalize_identity_text(
            component.as_deref(),
            "component",
            vp_protocol::types::AGENT_IDENTITY_MAX_COMPONENT_CHARS,
        )?;
        if value != existing.component {
            next.component = value;
            changed.push("component");
        }
    }
    if let Some(title) = &params.title {
        let value = normalize_identity_text(
            title.as_deref(),
            "title",
            vp_protocol::types::AGENT_IDENTITY_MAX_TITLE_CHARS,
        )?;
        if value != existing.title {
            next.title = value;
            changed.push("title");
        }
    }
    if let Some(parent) = &params.parent_identity_id {
        let value = normalize_identity_text(
            parent.as_deref(),
            "parent_identity_id",
            AGENT_IDENTITY_MAX_ID_CHARS,
        )?;
        if value != existing.parent_identity_id {
            next.parent_identity_id = value;
            changed.push("parent_identity_id");
        }
    }
    validate_identity_parent_tx(
        &tx,
        &existing.project_id,
        &existing.role,
        next.parent_identity_id.as_deref(),
    )?;
    if changed.is_empty() {
        return Ok(AgentTransferResult {
            identity: existing,
            changed: false,
            released_assignments: 0,
            archived_workspaces: 0,
            employment_event_id: None,
        });
    }

    let (active_assignments, active_workspaces, active_bindings) =
        identity_conflict_counts_tx(&tx, &existing.id)?;
    let component_changed = changed.contains(&"component");
    let needs_workspace_release = component_changed && active_workspaces > 0;
    if (active_assignments > 0 || active_bindings > 0 || needs_workspace_release) && !params.force {
        return Err(employment_error(
            agent_identity_codes::TRANSFER_CONFLICT,
            format!(
                "identity {} still has {} active assignment(s), {} workspace(s), and {} binding(s); release/archive them or pass force",
                existing.id, active_assignments, active_workspaces, active_bindings
            ),
        ));
    }
    let now = super::now_rfc3339();
    let mut released_assignments = 0;
    let mut archived_workspaces = 0;
    if params.force {
        let released = super::coordination::release_identity_assignments_tx(
            &tx,
            &existing.id,
            &operator,
            Some("transfer"),
            &now,
        )?;
        released_assignments = released.len() as u64;
        if component_changed || active_bindings > 0 {
            archived_workspaces = archive_identity_workspaces_tx(
                &tx,
                &existing.id,
                &now,
                &operator,
                Some("transfer"),
            )?
            .len() as u64;
        }
    }
    next.generation = existing.generation.saturating_add(1);
    next.updated_at = now;
    changed.push("generation");
    update_identity_and_event_tx(
        &tx,
        &existing,
        &next,
        &changed,
        vp_protocol::types::AGENT_IDENTITY_UPDATED_EVENT,
        serde_json::json!({
            "approved_by": operator,
            "approval_risk": "host_state",
        }),
    )?;
    let employment = insert_employment_event_tx(
        &tx,
        &existing.project_id,
        &existing.id,
        AGENT_TRANSFER_APPROVED_EVENT,
        serde_json::json!({
            "identity": next,
            "changed_fields": changed,
            "previous_generation": existing.generation,
            "approved_by": operator,
            "approval_risk": "host_state",
            "released_assignments": released_assignments,
            "archived_workspaces": archived_workspaces,
        }),
    )?;
    tx.commit()?;
    Ok(AgentTransferResult {
        identity: next,
        changed: true,
        released_assignments,
        archived_workspaces,
        employment_event_id: Some(employment.id),
    })
}

fn validate_successor_role_tx(
    tx: &Transaction<'_>,
    identity: &AgentIdentity,
    successor_id: &str,
) -> Result<()> {
    validate_identity_successor_tx(tx, &identity.project_id, &identity.id, successor_id)?;
    let successor: (String, String) = tx.query_row(
        "SELECT role, status FROM agent_identities WHERE id = ?1",
        params![successor_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let from = role_rank(&identity.role).unwrap_or(0);
    let to = role_rank(&successor.0).unwrap_or(0);
    if to + 1 < from || to > from {
        return Err(employment_error(
            agent_identity_codes::SUCCESSOR_INVALID,
            format!(
                "successor {} role {} is not the same role or the immediate subordinate role of {}",
                successor_id, successor.0, identity.role
            ),
        ));
    }
    if successor.1 == "retired" {
        return Err(employment_error(
            agent_identity_codes::SUCCESSOR_INVALID,
            "a retired identity cannot be a successor",
        ));
    }
    Ok(())
}

fn map_retirement_proposal(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRetirementProposal> {
    let reasons_json: String = row.get(4)?;
    let evidence_json: String = row.get(5)?;
    let performance_json: String = row.get(6)?;
    Ok(AgentRetirementProposal {
        id: row.get(0)?,
        project_id: row.get(1)?,
        identity_id: row.get(2)?,
        status: row.get(3)?,
        reasons: serde_json::from_str(&reasons_json).unwrap_or_default(),
        evidence: serde_json::from_str(&evidence_json).unwrap_or_default(),
        performance: serde_json::from_str(&performance_json).unwrap_or_default(),
        reason_code: row.get(7)?,
        proposed_at: row.get(8)?,
        proposed_by: row.get(9)?,
        decided_at: row.get(10)?,
        decided_by: row.get(11)?,
        decision_reason: row.get(12)?,
        successor_id: row.get(13)?,
        executed_at: row.get(14)?,
        origin: row.get(15)?,
    })
}

const RETIREMENT_PROPOSAL_SELECT: &str = "SELECT id, project_id, identity_id, status,
    reasons_json, evidence_json, performance_json, reason_code, proposed_at, proposed_by,
    decided_at, decided_by, decision_reason, successor_id, executed_at, origin
    FROM agent_retirement_proposals";

fn get_retirement_proposal_tx(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<Option<AgentRetirementProposal>> {
    tx.query_row(
        &format!("{RETIREMENT_PROPOSAL_SELECT} WHERE id = ?1"),
        params![id],
        map_retirement_proposal,
    )
    .optional()
    .map_err(Into::into)
}

fn pending_retirement_proposal_tx(
    tx: &Transaction<'_>,
    identity_id: &str,
) -> Result<Option<AgentRetirementProposal>> {
    tx.query_row(
        &format!(
            "{RETIREMENT_PROPOSAL_SELECT}
             WHERE identity_id = ?1 AND status = 'pending'
             ORDER BY proposed_at ASC, id ASC LIMIT 1"
        ),
        params![identity_id],
        map_retirement_proposal,
    )
    .optional()
    .map_err(Into::into)
}

fn insert_retirement_proposal_tx(
    tx: &Transaction<'_>,
    proposal: &AgentRetirementProposal,
) -> Result<()> {
    tx.execute(
        "INSERT INTO agent_retirement_proposals
         (id, project_id, identity_id, status, reason_code, reasons_json, evidence_json,
          performance_json, proposed_at, proposed_by, decided_at, decided_by, decision_reason,
          successor_id, executed_at, origin)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            proposal.id,
            proposal.project_id,
            proposal.identity_id,
            proposal.status,
            proposal.reason_code,
            serde_json::to_string(&proposal.reasons)?,
            serde_json::to_string(&proposal.evidence)?,
            serde_json::to_string(&proposal.performance)?,
            proposal.proposed_at,
            proposal.proposed_by,
            proposal.decided_at,
            proposal.decided_by,
            proposal.decision_reason,
            proposal.successor_id,
            proposal.executed_at,
            proposal.origin
        ],
    )?;
    Ok(())
}

fn update_retirement_proposal_tx(
    tx: &Transaction<'_>,
    proposal: &AgentRetirementProposal,
) -> Result<()> {
    tx.execute(
        "UPDATE agent_retirement_proposals
         SET status = ?1, decided_at = ?2, decided_by = ?3, decision_reason = ?4,
             successor_id = ?5, executed_at = ?6
         WHERE id = ?7",
        params![
            proposal.status,
            proposal.decided_at,
            proposal.decided_by,
            proposal.decision_reason,
            proposal.successor_id,
            proposal.executed_at,
            proposal.id
        ],
    )?;
    Ok(())
}

/// Retire an identity. Retirement is terminal, explicit, and preserves every
/// identity/memory/workspace/employment row. An approved proposal satisfies the
/// approval requirement; a pending proposal does not. `force` releases live
/// coordination/workspace state but never interrupts a provider stream and
/// never deletes branches or evidence.
pub fn retire_agent(store: &Store, params: &AgentRetireParams) -> Result<AgentRetireResult> {
    let operator = normalize_operator(params.operator.as_deref())?;
    let reason = normalize_reason(params.reason.as_deref(), "reason")?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = get_agent_identity_tx(&tx, params.identity_id.trim())?;
    if existing.origin != "local" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported identity is read-only until an explicit rehome",
        ));
    }
    if existing.status == "retired" {
        return Ok(AgentRetireResult {
            identity: existing,
            changed: false,
            proposal_id: params.proposal_id.clone(),
            released_assignments: 0,
            archived_workspaces: 0,
            employment_event_id: None,
        });
    }
    let identity_policy = agent_identity_policy_tx(&tx, &existing.project_id)?;
    let retirement_policy = retirement_policy_tx(&tx, &existing.project_id)?;
    let approval_required =
        identity_policy.retirement_requires_approval || retirement_policy.require_approval;

    let mut proposal = match params
        .proposal_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(proposal_id) => Some(get_retirement_proposal_tx(&tx, proposal_id)?.ok_or_else(
            || {
                employment_error(
                    agent_identity_codes::RETIREMENT_NOT_FOUND,
                    format!("retirement proposal not found: {proposal_id}"),
                )
            },
        )?),
        None => None,
    };
    if let Some(proposal) = &proposal {
        if proposal.origin == "imported" {
            return Err(employment_error(
                agent_identity_codes::IDENTITY_IMPORTED_READONLY,
                "an imported retirement proposal is archive evidence and cannot be executed locally",
            ));
        }
        if proposal.identity_id != existing.id || proposal.project_id != existing.project_id {
            return Err(employment_error(
                agent_identity_codes::RETIREMENT_CONFLICT,
                "retirement proposal does not belong to this identity",
            ));
        }
        match proposal.status.as_str() {
            "approved" | "pending" => {}
            "rejected" => {
                return Err(employment_error(
                    agent_identity_codes::RETIREMENT_CONFLICT,
                    "a rejected retirement proposal cannot be executed; create a new proposal",
                ));
            }
            "executed" => {
                return Err(employment_error(
                    agent_identity_codes::RETIREMENT_CONFLICT,
                    "retirement proposal was already executed",
                ));
            }
            "expired" => {
                return Err(employment_error(
                    agent_identity_codes::RETIREMENT_CONFLICT,
                    "retirement proposal expired; create a new proposal",
                ));
            }
            _ => {
                return Err(employment_error(
                    agent_identity_codes::RETIREMENT_CONFLICT,
                    "retirement proposal has an invalid status",
                ));
            }
        }
    }
    let approved_by_proposal = proposal
        .as_ref()
        .is_some_and(|proposal| proposal.status == "approved");
    if approval_required && !params.approved && !approved_by_proposal {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_APPROVAL_REQUIRED,
            "retirement requires explicit approved: true or an approved retirement proposal",
        ));
    }

    let successor = params
        .successor_id
        .clone()
        .or_else(|| proposal.as_ref().and_then(|p| p.successor_id.clone()));
    if let Some(successor_id) = successor.as_deref() {
        validate_successor_role_tx(&tx, &existing, successor_id)?;
    }

    let (active_assignments, active_workspaces, active_bindings) =
        identity_conflict_counts_tx(&tx, &existing.id)?;
    if (active_assignments > 0 || active_workspaces > 0 || active_bindings > 0) && !params.force {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_NOT_QUIESCENT,
            format!(
                "identity {} still has {} active assignment(s), {} workspace(s), and {} binding(s); quiesce it or pass force",
                existing.id, active_assignments, active_workspaces, active_bindings
            ),
        ));
    }
    let now = super::now_rfc3339();
    let mut released_assignments = 0;
    let mut archived_workspaces = 0;
    if params.force {
        let released = super::coordination::release_identity_assignments_tx(
            &tx,
            &existing.id,
            &operator,
            Some("retirement"),
            &now,
        )?;
        released_assignments = released.len() as u64;
        archived_workspaces =
            archive_identity_workspaces_tx(&tx, &existing.id, &now, &operator, Some("retirement"))?
                .len() as u64;
    }

    let mut next = existing.clone();
    next.status = "retired".to_string();
    next.updated_at = now.clone();
    next.retired_at = Some(now.clone());
    if successor.is_some() {
        next.successor_id = successor.clone();
    }
    let mut changed = vec!["status"];
    if next.successor_id != existing.successor_id {
        changed.push("successor_id");
    }
    update_identity_and_event_tx(
        &tx,
        &existing,
        &next,
        &changed,
        vp_protocol::types::AGENT_IDENTITY_RETIRED_EVENT,
        serde_json::json!({
            "approved_by": operator,
            "approval_risk": "host_state",
            "force": params.force,
            "reason": reason,
        }),
    )?;

    let proposal_id = proposal.as_ref().map(|proposal| proposal.id.clone());
    let mut executed_proposal = None;
    if let Some(mut proposal) = proposal.take() {
        if proposal.status == "pending" {
            // An explicit approved retire call is the decision; record it so the
            // proposal row never stays pending after execution.
            proposal.status = "approved".to_string();
            proposal.decided_at = Some(now.clone());
            proposal.decided_by = Some(operator.clone());
        }
        proposal.status = "executed".to_string();
        proposal.successor_id = successor.clone();
        proposal.executed_at = Some(now.clone());
        if proposal.decided_at.is_none() {
            proposal.decided_at = Some(now.clone());
            proposal.decided_by = Some(operator.clone());
        }
        update_retirement_proposal_tx(&tx, &proposal)?;
        insert_employment_event_tx(
            &tx,
            &existing.project_id,
            &existing.id,
            AGENT_RETIREMENT_APPROVED_EVENT,
            serde_json::json!({"proposal": proposal, "approved_by": operator, "approval_risk": "host_state"}),
        )?;
        executed_proposal = Some(proposal);
    }

    let employment = insert_employment_event_tx(
        &tx,
        &existing.project_id,
        &existing.id,
        AGENT_RETIREMENT_EXECUTED_EVENT,
        serde_json::json!({
            "identity": next,
            "successor_id": successor,
            "proposal_id": proposal_id,
            "proposal": executed_proposal.clone(),
            "approval_required": approval_required,
            "approved": params.approved,
            "approved_by": operator,
            "approval_risk": "host_state",
            "force": params.force,
            "reason": reason,
            "released_assignments": released_assignments,
            "archived_workspaces": archived_workspaces,
        }),
    )?;
    if let Some(proposal) = executed_proposal.as_mut() {
        // Use the append-only event timestamp as the authoritative execution
        // time so replay and the projected row are byte-equivalent.
        proposal.executed_at = Some(employment.created_at.clone());
        update_retirement_proposal_tx(&tx, proposal)?;
    }
    tx.commit()?;
    Ok(AgentRetireResult {
        identity: next,
        changed: true,
        proposal_id,
        released_assignments,
        archived_workspaces,
        employment_event_id: Some(employment.id),
    })
}

/// Persist one deterministic candidate as a pending proposal. The candidate is
/// computed by the pure core policy; this function only stores bounded evidence
/// and emits `agent.retirement.proposed`. It cannot retire anything.
pub fn create_retirement_proposal(
    store: &Store,
    candidate: &AgentRetirementCandidate,
    operator: Option<&str>,
) -> Result<(AgentRetirementProposal, Option<AgentEmploymentEvent>, bool)> {
    let operator = normalize_operator(operator)?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    project_exists_tx(&tx, &candidate.project_id)?;
    let policy = retirement_policy_tx(&tx, &candidate.project_id)?;
    if !policy.enabled {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_POLICY_DISABLED,
            "agent retirement policy is disabled",
        ));
    }
    if candidate.reasons.is_empty() {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_NOT_ELIGIBLE,
            "retirement candidate has no threshold breach",
        ));
    }
    let identity = get_agent_identity_tx(&tx, &candidate.identity_id)?;
    if identity.project_id != candidate.project_id {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_PROJECT_MISMATCH,
            "retirement candidate identity belongs to another project",
        ));
    }
    if identity.origin != "local" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported identity cannot receive a local retirement proposal",
        ));
    }
    if identity.status == "retired" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_RETIRED,
            "a retired identity cannot receive a retirement proposal",
        ));
    }
    if let Some(pending) = pending_retirement_proposal_tx(&tx, &identity.id)? {
        return Ok((pending, None, true));
    }
    let now = OffsetDateTime::now_utc();
    let cooldown = Duration::seconds(policy.cooldown_secs as i64);
    let last: Option<String> = tx
        .query_row(
            "SELECT MAX(proposed_at) FROM agent_retirement_proposals WHERE identity_id = ?1",
            params![identity.id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if let Some(last) = last.and_then(|value| parse_time(&value))
        && now - last < cooldown
    {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_COOLDOWN,
            format!(
                "retirement proposals for identity {} are in cooldown",
                identity.id
            ),
        ));
    }
    let now_text = format_time(now)?;
    let proposal = AgentRetirementProposal {
        id: Uuid::now_v7().to_string(),
        project_id: identity.project_id.clone(),
        identity_id: identity.id.clone(),
        status: "pending".to_string(),
        reason_code: candidate.reasons[0].code.clone(),
        reasons: candidate.reasons.iter().take(4).cloned().collect(),
        evidence: candidate
            .evidence
            .iter()
            .take(AGENT_EMPLOYMENT_MAX_EVIDENCE)
            .cloned()
            .collect(),
        performance: candidate.performance.clone(),
        proposed_at: now_text,
        proposed_by: operator.clone(),
        decided_at: None,
        decided_by: None,
        decision_reason: None,
        successor_id: None,
        executed_at: None,
        origin: "local".to_string(),
    };
    insert_retirement_proposal_tx(&tx, &proposal)?;
    let event = insert_employment_event_tx(
        &tx,
        &identity.project_id,
        &identity.id,
        AGENT_RETIREMENT_PROPOSED_EVENT,
        serde_json::json!({
            "proposal": proposal,
            "advisory": true,
            "approval_risk": "host_state",
        }),
    )?;
    tx.commit()?;
    Ok((proposal, Some(event), false))
}

fn decide_retirement_proposal(
    store: &Store,
    proposal_id: &str,
    approved: bool,
    reason: Option<&str>,
    operator: Option<&str>,
) -> Result<AgentRetirementDecisionResult> {
    let operator = normalize_operator(operator)?;
    let reason = normalize_reason(reason, "reason")?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut proposal = get_retirement_proposal_tx(&tx, proposal_id.trim())?.ok_or_else(|| {
        employment_error(
            agent_identity_codes::RETIREMENT_NOT_FOUND,
            format!("retirement proposal not found: {proposal_id}"),
        )
    })?;
    if proposal.origin == "imported" {
        return Err(employment_error(
            agent_identity_codes::IDENTITY_IMPORTED_READONLY,
            "an imported retirement proposal is archive evidence and cannot be decided locally",
        ));
    }
    if proposal.status != "pending" {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_CONFLICT,
            format!(
                "retirement proposal {} is already {}",
                proposal.id, proposal.status
            ),
        ));
    }
    proposal.status = if approved { "approved" } else { "rejected" }.to_string();
    proposal.decided_at = Some(super::now_rfc3339());
    proposal.decided_by = Some(operator.clone());
    proposal.decision_reason = reason.clone();
    update_retirement_proposal_tx(&tx, &proposal)?;
    insert_employment_event_tx(
        &tx,
        &proposal.project_id,
        &proposal.identity_id,
        if approved {
            AGENT_RETIREMENT_APPROVED_EVENT
        } else {
            AGENT_RETIREMENT_REJECTED_EVENT
        },
        serde_json::json!({
            "proposal": proposal,
            "approved_by": operator,
            "reason": reason,
            "advisory": true,
            "approval_risk": "host_state",
        }),
    )?;
    tx.commit()?;
    Ok(AgentRetirementDecisionResult {
        proposal,
        changed: true,
    })
}

pub fn approve_retirement_proposal(
    store: &Store,
    params: &vp_protocol::types::AgentRetirementProposalParams,
) -> Result<AgentRetirementDecisionResult> {
    decide_retirement_proposal(
        store,
        &params.proposal_id,
        true,
        None,
        params.operator.as_deref(),
    )
}

pub fn reject_retirement_proposal(
    store: &Store,
    params: &vp_protocol::types::AgentRetirementRejectParams,
) -> Result<AgentRetirementDecisionResult> {
    decide_retirement_proposal(
        store,
        &params.proposal_id,
        false,
        params.reason.as_deref(),
        params.operator.as_deref(),
    )
}

pub fn list_retirement_proposals(
    store: &Store,
    params: &AgentRetirementProposalListParams,
) -> Result<AgentRetirementProposalListResult> {
    let limit = params
        .limit
        .unwrap_or(100)
        .clamp(1, AGENT_EMPLOYMENT_MAX_LIST) as i64;
    if let Some(status) = &params.status
        && !AGENT_RETIREMENT_PROPOSAL_STATUSES.contains(&status.as_str())
    {
        return Err(employment_error(
            agent_identity_codes::RETIREMENT_POLICY_INVALID,
            format!("invalid retirement proposal status: {status}"),
        ));
    }
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "{RETIREMENT_PROPOSAL_SELECT}
         WHERE project_id = ?1
           AND (?2 IS NULL OR identity_id = ?2)
           AND (?3 IS NULL OR status = ?3)
         ORDER BY proposed_at DESC, id DESC LIMIT ?4"
    ))?;
    let proposals = statement
        .query_map(
            params![
                params.project_id.trim(),
                params.identity_id.as_deref(),
                params.status.as_deref(),
                limit
            ],
            map_retirement_proposal,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentRetirementProposalListResult { proposals })
}

pub fn get_retirement_policy(
    store: &Store,
    project_id: &str,
) -> Result<AgentRetirementPolicyResult> {
    let mut conn = store.lock()?;
    let tx = conn.transaction()?;
    let policy = retirement_policy_tx(&tx, project_id.trim())?;
    tx.commit()?;
    Ok(AgentRetirementPolicyResult {
        project_id: project_id.trim().to_string(),
        policy,
    })
}

pub fn set_retirement_policy(
    store: &Store,
    project_id: &str,
    policy: &AgentRetirementPolicy,
) -> Result<AgentRetirementPolicyResult> {
    let policy = policy.normalized().map_err(|error| {
        employment_error(agent_identity_codes::RETIREMENT_POLICY_INVALID, error)
    })?;
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    project_exists_tx(&tx, project_id.trim())?;
    tx.execute(
        "INSERT INTO project_agent_retirement_policies (project_id, policy, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(project_id) DO UPDATE SET policy = excluded.policy,
             updated_at = excluded.updated_at",
        params![
            project_id.trim(),
            serde_json::to_string(&policy)?,
            super::now_rfc3339()
        ],
    )?;
    tx.commit()?;
    Ok(AgentRetirementPolicyResult {
        project_id: project_id.trim().to_string(),
        policy,
    })
}

pub fn list_employment_events(
    store: &Store,
    project_id: &str,
    limit: Option<u32>,
) -> Result<Vec<AgentEmploymentEvent>> {
    let limit = limit.unwrap_or(100_000).clamp(1, 100_000) as i64;
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT id, project_id, seq, identity_id, kind, payload, created_at
         FROM agent_employment_events WHERE project_id = ?1
         ORDER BY seq ASC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id.trim(), limit], |row| {
        let payload: String = row.get(5)?;
        Ok(AgentEmploymentEvent {
            id: row.get(0)?,
            project_id: row.get(1)?,
            seq: row.get::<_, i64>(2)? as u64,
            identity_id: row.get(3)?,
            kind: row.get(4)?,
            payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
            created_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// Derived performance ledger
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PerfAgent {
    id: String,
    session_id: String,
    name: String,
    role: String,
    department: Option<String>,
    title: Option<String>,
    identity_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PerfUsage {
    session_id: String,
    agent_id: String,
    role: String,
    department: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    estimated: bool,
    cost_usd: f64,
    created_at: String,
}

#[derive(Debug, Clone)]
struct PerfMerge {
    id: String,
    session_id: String,
    agent_id: String,
    agent_name: String,
    verifier_approved: Option<bool>,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct PerfTaskOutcome {
    session_id: String,
    task_id: String,
    status: String,
    identity_id: Option<String>,
    at: String,
    seq: u64,
}

/// Compute the project performance ledger without writing it. This is a pure
/// read over the existing evidence tables; `agent.performance` uses it directly
/// so a read never mutates the database.
pub fn compute_agent_performance(
    store: &Store,
    project_id: &str,
    lookback_days: u32,
    now: OffsetDateTime,
) -> Result<Vec<AgentPerformance>> {
    if !(AGENT_RETIREMENT_MIN_LOOKBACK_DAYS..=AGENT_RETIREMENT_MAX_LOOKBACK_DAYS)
        .contains(&lookback_days)
    {
        return Err(employment_error(
            agent_identity_codes::PERFORMANCE_INVALID,
            format!(
                "lookback_days must be {}..={}",
                AGENT_RETIREMENT_MIN_LOOKBACK_DAYS, AGENT_RETIREMENT_MAX_LOOKBACK_DAYS
            ),
        ));
    }
    let cutoff = format_time(now - Duration::days(lookback_days as i64))?;
    let now_text = format_time(now)?;
    let conn = store.lock()?;

    let identities = {
        let mut statement = conn.prepare(&format!(
            "SELECT {} FROM agent_identities WHERE project_id = ?1 ORDER BY created_at ASC, id ASC",
            super::AGENT_IDENTITY_COLUMNS
        ))?;
        statement
            .query_map(params![project_id], super::map_agent_identity)?
            .collect::<Result<Vec<_>, _>>()?
    };
    if identities.is_empty() {
        return Ok(Vec::new());
    }
    let identity_ids = identities
        .iter()
        .map(|identity| identity.id.clone())
        .collect::<HashSet<_>>();
    let identity_by_id = identities
        .iter()
        .map(|identity| (identity.id.clone(), identity.clone()))
        .collect::<HashMap<_, _>>();

    let assignments = {
        let mut statement = conn.prepare(&format!(
            "{ASSIGNMENT_SELECT} WHERE s.project_id = ?1 ORDER BY a.session_id ASC, a.identity_id ASC LIMIT ?2"
        ))?;
        statement
            .query_map(
                params![project_id, MAX_PERFORMANCE_EVIDENCE_ROWS as i64],
                map_assignment,
            )?
            .collect::<Result<Vec<_>, _>>()?
    };

    let agents = {
        let mut statement = conn.prepare(
            "SELECT a.id, a.session_id, a.name, a.role, a.department, a.title, a.identity_id
             FROM agents a JOIN sessions s ON s.id = a.session_id
             WHERE s.project_id = ?1
             ORDER BY a.created_at ASC, a.id ASC LIMIT ?2",
        )?;
        statement
            .query_map(
                params![project_id, MAX_PERFORMANCE_AGENT_ROWS as i64],
                |row| {
                    Ok(PerfAgent {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        name: row.get(2)?,
                        role: row.get(3)?,
                        department: row.get(4)?,
                        title: row.get(5)?,
                        identity_id: row.get(6)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };

    let task_rows = {
        let mut statement = conn.prepare(
            "SELECT e.session_id, e.seq, e.type, e.payload, e.created_at
             FROM events e JOIN sessions s ON s.id = e.session_id
             WHERE s.project_id = ?1 AND e.created_at >= ?2
               AND e.type IN ('plan_task.finished', 'plan_task.retry_scheduled', 'failure.attribution')
             ORDER BY e.created_at ASC, e.session_id ASC, e.seq ASC LIMIT ?3",
        )?;
        statement
            .query_map(
                params![project_id, cutoff, MAX_PERFORMANCE_TASK_ROWS as i64],
                |row| {
                    let payload: String = row.get(3)?;
                    Ok(PerfEventRow {
                        session_id: row.get(0)?,
                        seq: row.get::<_, i64>(1)? as u64,
                        kind: row.get(2)?,
                        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                        created_at: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };

    let usage_rows = {
        let mut statement = conn.prepare(
            "SELECT u.session_id, u.agent_id, u.role, u.department, u.input_tokens,
                    u.output_tokens, u.cache_read_tokens, u.estimated, u.cost_usd, u.created_at
             FROM usage_ledger u JOIN sessions s ON s.id = u.session_id
             WHERE s.project_id = ?1 AND u.created_at >= ?2
             ORDER BY u.created_at ASC, u.id ASC LIMIT ?3",
        )?;
        statement
            .query_map(
                params![project_id, cutoff, MAX_PERFORMANCE_USAGE_ROWS as i64],
                |row| {
                    Ok(PerfUsage {
                        session_id: row.get(0)?,
                        agent_id: row.get(1)?,
                        role: row.get(2)?,
                        department: row.get(3)?,
                        input_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                        output_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                        cache_read_tokens: row.get::<_, i64>(6)?.max(0) as u64,
                        estimated: row.get::<_, i64>(7)? != 0,
                        cost_usd: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };

    let merge_rows = {
        let mut statement = conn.prepare(
            "SELECT m.id, m.session_id, m.agent_id, m.agent_name, m.verifier_approved, m.updated_at
             FROM merge_queue m JOIN sessions s ON s.id = m.session_id
             WHERE s.project_id = ?1 AND m.updated_at >= ?2
             ORDER BY m.updated_at ASC, m.id ASC LIMIT ?3",
        )?;
        statement
            .query_map(
                params![project_id, cutoff, MAX_PERFORMANCE_MERGE_ROWS as i64],
                |row| {
                    Ok(PerfMerge {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        agent_id: row.get(2)?,
                        agent_name: row.get(3)?,
                        verifier_approved: row.get::<_, Option<i64>>(4)?.map(|value| value != 0),
                        updated_at: row.get(5)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    drop(conn);

    let agent_by_key = agents
        .iter()
        .map(|agent| ((agent.session_id.clone(), agent.id.clone()), agent.clone()))
        .collect::<HashMap<_, _>>();
    let mut agent_by_name: HashMap<(String, String), Vec<PerfAgent>> = HashMap::new();
    for agent in &agents {
        agent_by_name
            .entry((agent.session_id.clone(), agent.name.clone()))
            .or_default()
            .push(agent.clone());
    }
    let mut assignments_by_session: HashMap<String, Vec<vp_protocol::types::AgentAssignment>> =
        HashMap::new();
    let mut assignments_by_identity: HashMap<String, Vec<vp_protocol::types::AgentAssignment>> =
        HashMap::new();
    for assignment in assignments {
        assignments_by_identity
            .entry(assignment.identity_id.clone())
            .or_default()
            .push(assignment.clone());
        assignments_by_session
            .entry(assignment.session_id.clone())
            .or_default()
            .push(assignment);
    }

    let resolve = |session_id: &str,
                   agent_id: Option<&str>,
                   agent_name: Option<&str>,
                   role: Option<&str>,
                   department: Option<&str>,
                   title: Option<&str>|
     -> Option<String> {
        if let Some(agent_id) = agent_id
            && let Some(agent) = agent_by_key.get(&(session_id.to_string(), agent_id.to_string()))
        {
            if let Some(identity_id) = &agent.identity_id
                && identity_ids.contains(identity_id)
            {
                return Some(identity_id.clone());
            }
            // A historical run may predate identity binding. Its own role,
            // department, and title are still useful attribution evidence when
            // exactly one assigned identity matches.
            if let Some(session_assignments) = assignments_by_session.get(session_id) {
                let candidates = session_assignments
                    .iter()
                    .filter_map(|assignment| identity_by_id.get(&assignment.identity_id))
                    .filter(|identity| identity.role.eq_ignore_ascii_case(&agent.role))
                    .filter(|identity| {
                        agent.department.as_deref().is_none_or(|department| {
                            identity
                                .department
                                .as_deref()
                                .is_some_and(|value| value.eq_ignore_ascii_case(department))
                        })
                    })
                    .filter(|identity| {
                        agent.title.as_deref().is_none_or(|title| {
                            identity
                                .title
                                .as_deref()
                                .is_some_and(|value| value.eq_ignore_ascii_case(title))
                        })
                    })
                    .collect::<Vec<_>>();
                if candidates.len() == 1 {
                    return Some(candidates[0].id.clone());
                }
            }
        }
        if let Some(agent_name) = agent_name
            && let Some(candidates) =
                agent_by_name.get(&(session_id.to_string(), agent_name.to_string()))
            && candidates.len() == 1
            && let Some(identity_id) = &candidates[0].identity_id
            && identity_ids.contains(identity_id)
        {
            return Some(identity_id.clone());
        }
        let session_assignments = assignments_by_session.get(session_id)?;
        let mut candidates = session_assignments
            .iter()
            .filter_map(|assignment| identity_by_id.get(&assignment.identity_id))
            .filter(|identity| role.is_none_or(|role| identity.role.eq_ignore_ascii_case(role)))
            .collect::<Vec<_>>();
        if let Some(department) = department {
            candidates.retain(|identity| {
                identity
                    .department
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(department))
            });
        }
        if let Some(title) = title {
            candidates.retain(|identity| {
                identity
                    .title
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(title))
            });
        }
        if candidates.len() == 1 {
            return Some(candidates[0].id.clone());
        }
        None
    };

    // Per-identity accumulators.
    struct Acc {
        performance: AgentPerformance,
        reason_counts: BTreeMap<String, u64>,
        evidence: Vec<AgentPerformanceEvidence>,
        usage_sessions: HashSet<String>,
    }
    impl Acc {
        fn new(identity: &AgentIdentity, lookback_days: u32, now: &str) -> Self {
            Self {
                performance: AgentPerformance {
                    project_id: identity.project_id.clone(),
                    identity_id: identity.id.clone(),
                    role: identity.role.clone(),
                    status: identity.status.clone(),
                    lookback_days,
                    computed_at: now.to_string(),
                    ..AgentPerformance::default()
                },
                reason_counts: BTreeMap::new(),
                evidence: Vec::new(),
                usage_sessions: HashSet::new(),
            }
        }
        fn push_evidence(&mut self, mut evidence: AgentPerformanceEvidence) {
            if self.evidence.len() >= 4_096 {
                return;
            }
            // Evidence pointers are ids/timestamps, never content. Bound each
            // field so a malformed event cannot inflate a proposal payload.
            evidence.kind = evidence.kind.chars().take(40).collect();
            evidence.reference = evidence.reference.chars().take(200).collect();
            evidence.at = evidence
                .at
                .map(|value| value.chars().take(40).collect::<String>());
            evidence.note = evidence
                .note
                .map(|value| value.chars().take(200).collect::<String>());
            if evidence.kind.is_empty() || evidence.reference.is_empty() {
                return;
            }
            self.evidence.push(evidence);
        }
    }
    let mut accs = identities
        .iter()
        .map(|identity| {
            (
                identity.id.clone(),
                Acc::new(identity, lookback_days, &now_text),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut latest_tasks: BTreeMap<(String, String), PerfTaskOutcome> = BTreeMap::new();
    let mut attribution_ids: BTreeSet<String> = BTreeSet::new();

    for (identity_id, rows) in &assignments_by_identity {
        let Some(acc) = accs.get_mut(identity_id) else {
            continue;
        };
        acc.performance.assignment_count = rows.len() as u64;
        let sessions = rows
            .iter()
            .map(|assignment| assignment.session_id.clone())
            .collect::<BTreeSet<_>>();
        acc.performance.session_count = sessions.len() as u64;
        for assignment in rows {
            let at = if assignment.heartbeat_at.is_empty() {
                Some(assignment.assigned_at.clone())
            } else {
                Some(assignment.heartbeat_at.clone())
            };
            update_min(
                &mut acc.performance.first_active_at,
                &assignment.assigned_at,
            );
            if let Some(at) = at {
                update_max(&mut acc.performance.last_active_at, &at);
            }
            if let Some(released) = &assignment.released_at {
                update_max(&mut acc.performance.last_active_at, released);
            }
            acc.push_evidence(AgentPerformanceEvidence {
                kind: "assignment".to_string(),
                reference: format!("{}:{}", assignment.session_id, assignment.status),
                reason: Some(perf_reasons::ASSIGNMENT.to_string()),
                at: Some(assignment.assigned_at.clone()),
                hash: None,
                note: Some(format!("role {}", assignment.role)),
            });
        }
    }

    for row in &task_rows {
        match row.kind.as_str() {
            "plan_task.finished" => {
                let Some(task_id) = text_value(&row.payload, "task_id") else {
                    continue;
                };
                let status = text_value(&row.payload, "task_status").unwrap_or("unknown");
                let identity_id = resolve(
                    &row.session_id,
                    text_value(&row.payload, "agent_id"),
                    text_value(&row.payload, "agent_name"),
                    text_value(&row.payload, "agent_role")
                        .or_else(|| text_value(&row.payload, "role")),
                    text_value(&row.payload, "department"),
                    text_value(&row.payload, "agent_title"),
                )
                .or_else(|| text_value(&row.payload, "identity_id").map(str::to_string));
                let outcome = PerfTaskOutcome {
                    session_id: row.session_id.clone(),
                    task_id: task_id.to_string(),
                    status: status.to_string(),
                    identity_id,
                    at: row.created_at.clone(),
                    seq: row.seq,
                };
                // Latest final outcome wins for one task; keep one row per task.
                let key = (row.session_id.clone(), task_id.to_string());
                match latest_tasks.get(&key) {
                    Some(existing)
                        if (existing.at.as_str(), existing.seq)
                            > (outcome.at.as_str(), outcome.seq) => {}
                    _ => {
                        latest_tasks.insert(key, outcome);
                    }
                }
            }
            "plan_task.retry_scheduled" => {
                let identity_id = resolve(
                    &row.session_id,
                    text_value(&row.payload, "agent_id"),
                    text_value(&row.payload, "agent_name"),
                    text_value(&row.payload, "agent_role")
                        .or_else(|| text_value(&row.payload, "role")),
                    text_value(&row.payload, "department"),
                    text_value(&row.payload, "agent_title"),
                )
                .or_else(|| text_value(&row.payload, "identity_id").map(str::to_string));
                if let Some(identity_id) = identity_id
                    && let Some(acc) = accs.get_mut(&identity_id)
                {
                    acc.performance.retries = acc.performance.retries.saturating_add(1);
                }
            }
            "failure.attribution" => {
                let Some(attribution) = row.payload.get("attribution") else {
                    continue;
                };
                let attribution_id = text_value(attribution, "attribution_id")
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{}:{}", row.session_id, row.seq));
                if !attribution_ids.insert(attribution_id.clone()) {
                    continue;
                }
                let reason = text_value(attribution, "reason_code").unwrap_or("unknown");
                let mut attributed = BTreeSet::new();
                for agent in attribution
                    .get("responsible_agents")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(identity_id) = text_value(agent, "identity_id")
                        && identity_ids.contains(identity_id)
                    {
                        attributed.insert(identity_id.to_string());
                    }
                }
                for finding in attribution
                    .get("findings")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    for agent in finding
                        .get("agents")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(identity_id) = text_value(agent, "identity_id")
                            && identity_ids.contains(identity_id)
                        {
                            attributed.insert(identity_id.to_string());
                        }
                    }
                }
                for identity_id in attributed {
                    let Some(acc) = accs.get_mut(&identity_id) else {
                        continue;
                    };
                    acc.performance.diagnosis_findings =
                        acc.performance.diagnosis_findings.saturating_add(1);
                    *acc.reason_counts.entry(reason.to_string()).or_insert(0) += 1;
                    if reason == agent_retirement_reasons::UNSUBSTANTIATED_HANDOFFS {
                        acc.performance.unsubstantiated_handoffs =
                            acc.performance.unsubstantiated_handoffs.saturating_add(1);
                    }
                    if reason == agent_retirement_reasons::FALSE_CONSENSUS {
                        acc.performance.false_consensus =
                            acc.performance.false_consensus.saturating_add(1);
                    }
                    acc.push_evidence(AgentPerformanceEvidence {
                        kind: "attribution".to_string(),
                        reference: attribution_id.clone(),
                        reason: Some(reason.to_string()),
                        at: Some(row.created_at.clone()),
                        hash: None,
                        note: Some("advisory diagnosis".to_string()),
                    });
                }
            }
            _ => {}
        }
    }

    // Task outcome counts are per final task, not per attempt.
    for outcome in latest_tasks.values() {
        let Some(identity_id) = &outcome.identity_id else {
            continue;
        };
        let Some(acc) = accs.get_mut(identity_id) else {
            continue;
        };
        match outcome.status.as_str() {
            "completed" => {
                acc.performance.completed_tasks = acc.performance.completed_tasks.saturating_add(1);
            }
            "failed" => {
                acc.performance.failed_tasks = acc.performance.failed_tasks.saturating_add(1);
            }
            "cancelled" => {
                acc.performance.cancelled_tasks = acc.performance.cancelled_tasks.saturating_add(1);
            }
            "blocked" => {
                acc.performance.blocked_tasks = acc.performance.blocked_tasks.saturating_add(1);
            }
            "skipped" => {
                acc.performance.skipped_tasks = acc.performance.skipped_tasks.saturating_add(1);
            }
            _ => {}
        }
        update_max(&mut acc.performance.last_active_at, &outcome.at);
        acc.push_evidence(AgentPerformanceEvidence {
            kind: "task".to_string(),
            reference: outcome.task_id.clone(),
            reason: Some(if outcome.status == "completed" {
                perf_reasons::TASK_COMPLETED.to_string()
            } else if outcome.status == "failed" {
                perf_reasons::TASK_FAILED.to_string()
            } else {
                "task_outcome".to_string()
            }),
            at: Some(outcome.at.clone()),
            hash: None,
            note: Some(format!("status {}", outcome.status)),
        });
    }

    for row in &merge_rows {
        let identity_id = resolve(
            &row.session_id,
            Some(&row.agent_id),
            Some(&row.agent_name),
            Some("worker"),
            None,
            None,
        );
        let Some(identity_id) = identity_id else {
            continue;
        };
        let Some(acc) = accs.get_mut(&identity_id) else {
            continue;
        };
        match row.verifier_approved {
            Some(true) => {
                acc.performance.verifier_approvals =
                    acc.performance.verifier_approvals.saturating_add(1);
                acc.push_evidence(AgentPerformanceEvidence {
                    kind: "merge".to_string(),
                    reference: row.id.clone(),
                    reason: Some(perf_reasons::VERIFIER_APPROVAL.to_string()),
                    at: Some(row.updated_at.clone()),
                    hash: None,
                    note: None,
                });
            }
            Some(false) => {
                acc.performance.verifier_rejections =
                    acc.performance.verifier_rejections.saturating_add(1);
                acc.push_evidence(AgentPerformanceEvidence {
                    kind: "merge".to_string(),
                    reference: row.id.clone(),
                    reason: Some(perf_reasons::VERIFIER_REJECTION.to_string()),
                    at: Some(row.updated_at.clone()),
                    hash: None,
                    note: None,
                });
            }
            None => {}
        }
        update_max(&mut acc.performance.last_active_at, &row.updated_at);
    }

    for row in &usage_rows {
        let identity_id = resolve(
            &row.session_id,
            Some(&row.agent_id),
            None,
            Some(&row.role),
            row.department.as_deref(),
            None,
        );
        let Some(identity_id) = identity_id else {
            continue;
        };
        let Some(acc) = accs.get_mut(&identity_id) else {
            continue;
        };
        acc.performance.input_tokens = acc
            .performance
            .input_tokens
            .saturating_add(row.input_tokens);
        acc.performance.output_tokens = acc
            .performance
            .output_tokens
            .saturating_add(row.output_tokens);
        acc.performance.cache_read_tokens = acc
            .performance
            .cache_read_tokens
            .saturating_add(row.cache_read_tokens);
        if row.estimated {
            acc.performance.estimated_usage_calls =
                acc.performance.estimated_usage_calls.saturating_add(1);
        }
        acc.performance.cost_usd += row.cost_usd;
        update_max(&mut acc.performance.last_active_at, &row.created_at);
        if acc.usage_sessions.insert(row.session_id.clone()) {
            acc.push_evidence(AgentPerformanceEvidence {
                kind: "usage".to_string(),
                reference: row.session_id.clone(),
                reason: Some(perf_reasons::USAGE.to_string()),
                at: Some(row.created_at.clone()),
                hash: None,
                note: Some(format!("role {}", row.role)),
            });
        }
    }

    let mut rows = Vec::with_capacity(accs.len());
    for identity in &identities {
        let Some(mut acc) = accs.remove(&identity.id) else {
            continue;
        };
        // Consecutive failures: the trailing run of `failed` final outcomes.
        let outcomes = latest_tasks
            .values()
            .filter(|outcome| outcome.identity_id.as_deref() == Some(identity.id.as_str()))
            .collect::<Vec<_>>();
        let mut sorted = outcomes;
        sorted.sort_by(|left, right| {
            (
                left.at.as_str(),
                left.session_id.as_str(),
                left.task_id.as_str(),
            )
                .cmp(&(
                    right.at.as_str(),
                    right.session_id.as_str(),
                    right.task_id.as_str(),
                ))
        });
        let mut consecutive = 0u64;
        for outcome in sorted.iter().rev() {
            if outcome.status == "failed" {
                consecutive += 1;
            } else {
                break;
            }
        }
        acc.performance.consecutive_failures = consecutive;
        acc.performance.reason_counts = acc
            .reason_counts
            .into_iter()
            .map(|(code, count)| AgentPerformanceReasonCount { code, count })
            .collect();
        if acc.performance.reason_counts.len() > AGENT_PERFORMANCE_MAX_REASON_COUNTS {
            acc.performance.reason_counts.sort_by(|left, right| {
                right
                    .count
                    .cmp(&left.count)
                    .then_with(|| left.code.cmp(&right.code))
            });
            acc.performance
                .reason_counts
                .truncate(AGENT_PERFORMANCE_MAX_REASON_COUNTS);
        }
        let mut evidence = std::mem::take(&mut acc.evidence);
        evidence.sort_by(|left, right| {
            right
                .at
                .as_deref()
                .unwrap_or_default()
                .cmp(left.at.as_deref().unwrap_or_default())
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.reference.cmp(&right.reference))
        });
        evidence.truncate(AGENT_PERFORMANCE_MAX_EVIDENCE);
        evidence.sort_by(|left, right| {
            left.at
                .as_deref()
                .unwrap_or_default()
                .cmp(right.at.as_deref().unwrap_or_default())
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.reference.cmp(&right.reference))
        });
        acc.performance.evidence = evidence;
        acc.performance.cost_usd = if acc.performance.cost_usd.is_finite() {
            acc.performance.cost_usd.max(0.0)
        } else {
            0.0
        };
        rows.push(acc.performance);
    }
    rows.sort_by(|left, right| left.identity_id.cmp(&right.identity_id));
    Ok(rows)
}

struct PerfEventRow {
    session_id: String,
    seq: u64,
    kind: String,
    payload: Value,
    created_at: String,
}

fn text_value<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn update_min(target: &mut Option<String>, candidate: &str) {
    if candidate.is_empty() {
        return;
    }
    match target {
        Some(current) if current.as_str() <= candidate => {}
        _ => *target = Some(candidate.to_string()),
    }
}

fn update_max(target: &mut Option<String>, candidate: &str) {
    if candidate.is_empty() {
        return;
    }
    match target {
        Some(current) if current.as_str() >= candidate => {}
        _ => *target = Some(candidate.to_string()),
    }
}

/// Compute and persist the performance projection. Used by `db-rebuild` and an
/// explicit replay repair, never by an ordinary read.
pub fn rebuild_agent_performance(
    store: &Store,
    project_id: &str,
    lookback_days: u32,
    now: OffsetDateTime,
) -> Result<Vec<AgentPerformance>> {
    let rows = compute_agent_performance(store, project_id, lookback_days, now)?;
    replace_agent_performance_projection(store, project_id, &rows)?;
    Ok(rows)
}

pub fn replace_agent_performance_projection(
    store: &Store,
    project_id: &str,
    rows: &[AgentPerformance],
) -> Result<()> {
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM agent_performance WHERE project_id = ?1",
        params![project_id],
    )?;
    for row in rows {
        if row.project_id != project_id {
            return Err(employment_error(
                agent_identity_codes::PERFORMANCE_INVALID,
                "performance row belongs to another project",
            ));
        }
        insert_agent_performance_row_tx(&tx, row)?;
    }
    tx.commit()?;
    Ok(())
}

fn insert_agent_performance_row_tx(tx: &Transaction<'_>, row: &AgentPerformance) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO agent_performance
         (project_id, identity_id, role, status, lookback_days, completed_tasks, failed_tasks,
          cancelled_tasks, blocked_tasks, skipped_tasks, consecutive_failures, retries,
          verifier_approvals, verifier_rejections, unsubstantiated_handoffs, false_consensus,
          diagnosis_findings, assignment_count, session_count, input_tokens, output_tokens,
          cache_read_tokens, estimated_usage_calls, cost_usd, first_active_at, last_active_at,
          reason_counts_json, evidence_json, computed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)",
        params![
            row.project_id,
            row.identity_id,
            row.role,
            row.status,
            row.lookback_days as i64,
            row.completed_tasks as i64,
            row.failed_tasks as i64,
            row.cancelled_tasks as i64,
            row.blocked_tasks as i64,
            row.skipped_tasks as i64,
            row.consecutive_failures as i64,
            row.retries as i64,
            row.verifier_approvals as i64,
            row.verifier_rejections as i64,
            row.unsubstantiated_handoffs as i64,
            row.false_consensus as i64,
            row.diagnosis_findings as i64,
            row.assignment_count as i64,
            row.session_count as i64,
            row.input_tokens as i64,
            row.output_tokens as i64,
            row.cache_read_tokens as i64,
            row.estimated_usage_calls as i64,
            row.cost_usd,
            row.first_active_at,
            row.last_active_at,
            serde_json::to_string(&row.reason_counts)?,
            serde_json::to_string(&row.evidence)?,
            row.computed_at
        ],
    )?;
    Ok(())
}

pub fn list_agent_performance(
    store: &Store,
    project_id: &str,
    limit: Option<u32>,
) -> Result<Vec<AgentPerformance>> {
    let limit = limit.unwrap_or(500).clamp(1, AGENT_EMPLOYMENT_MAX_LIST) as i64;
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT project_id, identity_id, role, status, lookback_days, completed_tasks,
                failed_tasks, cancelled_tasks, blocked_tasks, skipped_tasks, consecutive_failures,
                retries, verifier_approvals, verifier_rejections, unsubstantiated_handoffs,
                false_consensus, diagnosis_findings, assignment_count, session_count,
                input_tokens, output_tokens, cache_read_tokens, estimated_usage_calls, cost_usd,
                first_active_at, last_active_at, reason_counts_json, evidence_json, computed_at
         FROM agent_performance WHERE project_id = ?1
         ORDER BY identity_id ASC LIMIT ?2",
    )?;
    let rows = statement.query_map(params![project_id.trim(), limit], map_agent_performance)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_agent_performance(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentPerformance> {
    let reason_counts: String = row.get(26)?;
    let evidence: String = row.get(27)?;
    Ok(AgentPerformance {
        project_id: row.get(0)?,
        identity_id: row.get(1)?,
        role: row.get(2)?,
        status: row.get(3)?,
        lookback_days: row.get::<_, i64>(4)?.max(0) as u32,
        completed_tasks: row.get::<_, i64>(5)?.max(0) as u64,
        failed_tasks: row.get::<_, i64>(6)?.max(0) as u64,
        cancelled_tasks: row.get::<_, i64>(7)?.max(0) as u64,
        blocked_tasks: row.get::<_, i64>(8)?.max(0) as u64,
        skipped_tasks: row.get::<_, i64>(9)?.max(0) as u64,
        consecutive_failures: row.get::<_, i64>(10)?.max(0) as u64,
        retries: row.get::<_, i64>(11)?.max(0) as u64,
        verifier_approvals: row.get::<_, i64>(12)?.max(0) as u64,
        verifier_rejections: row.get::<_, i64>(13)?.max(0) as u64,
        unsubstantiated_handoffs: row.get::<_, i64>(14)?.max(0) as u64,
        false_consensus: row.get::<_, i64>(15)?.max(0) as u64,
        diagnosis_findings: row.get::<_, i64>(16)?.max(0) as u64,
        assignment_count: row.get::<_, i64>(17)?.max(0) as u64,
        session_count: row.get::<_, i64>(18)?.max(0) as u64,
        input_tokens: row.get::<_, i64>(19)?.max(0) as u64,
        output_tokens: row.get::<_, i64>(20)?.max(0) as u64,
        cache_read_tokens: row.get::<_, i64>(21)?.max(0) as u64,
        estimated_usage_calls: row.get::<_, i64>(22)?.max(0) as u64,
        cost_usd: row.get(23)?,
        first_active_at: row.get(24)?,
        last_active_at: row.get(25)?,
        reason_counts: serde_json::from_str(&reason_counts).unwrap_or_default(),
        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
        computed_at: row.get(28)?,
    })
}

pub fn performance_summary(
    store: &Store,
    params: &AgentPerformanceSummaryParams,
    now: OffsetDateTime,
) -> Result<AgentPerformanceSummaryResult> {
    let policy = get_retirement_policy(store, &params.project_id)?.policy;
    let lookback = params.lookback_days.unwrap_or(policy.lookback_days);
    let mut rows = compute_agent_performance(store, &params.project_id, lookback, now)?;
    let mut totals = AgentPerformanceTotals {
        identities: rows.len() as u64,
        ..AgentPerformanceTotals::default()
    };
    for row in &rows {
        totals.completed_tasks = totals.completed_tasks.saturating_add(row.completed_tasks);
        totals.failed_tasks = totals.failed_tasks.saturating_add(row.failed_tasks);
        totals.verifier_approvals = totals
            .verifier_approvals
            .saturating_add(row.verifier_approvals);
        totals.verifier_rejections = totals
            .verifier_rejections
            .saturating_add(row.verifier_rejections);
        totals.input_tokens = totals.input_tokens.saturating_add(row.input_tokens);
        totals.output_tokens = totals.output_tokens.saturating_add(row.output_tokens);
        totals.cost_usd += row.cost_usd;
    }
    let limit = params
        .limit
        .unwrap_or(AGENT_EMPLOYMENT_MAX_LIST)
        .clamp(1, AGENT_EMPLOYMENT_MAX_LIST) as usize;
    rows.truncate(limit);
    Ok(AgentPerformanceSummaryResult {
        project_id: params.project_id.clone(),
        computed_at: format_time(now)?,
        lookback_days: lookback,
        retirement_policy_enabled: policy.enabled,
        identities: rows,
        totals,
    })
}

pub fn performance_replay(
    store: &Store,
    project_id: &str,
    repair: bool,
    now: OffsetDateTime,
) -> Result<AgentPerformanceReplayReport> {
    let policy = get_retirement_policy(store, project_id)?.policy;
    let expected = compute_agent_performance(store, project_id, policy.lookback_days, now)?;
    let actual = list_agent_performance(store, project_id, None)?;
    let mut mismatches = compare_performance_rows(&expected, &actual);
    let mut repaired = false;
    if repair && !mismatches.is_empty() {
        replace_agent_performance_projection(store, project_id, &expected)?;
        let actual = list_agent_performance(store, project_id, None)?;
        mismatches = compare_performance_rows(&expected, &actual);
        repaired = true;
    }
    Ok(AgentPerformanceReplayReport {
        project_id: project_id.to_string(),
        rows: expected.len() as u64,
        consistent: mismatches.is_empty(),
        repaired,
        mismatches,
    })
}

fn performance_equivalent(left: &AgentPerformance, right: &AgentPerformance) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.computed_at.clear();
    right.computed_at.clear();
    left == right
}

fn compare_performance_rows(
    expected: &[AgentPerformance],
    actual: &[AgentPerformance],
) -> Vec<String> {
    let expected_by_id = expected
        .iter()
        .map(|row| (row.identity_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let actual_by_id = actual
        .iter()
        .map(|row| (row.identity_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut mismatches = Vec::new();
    for id in expected_by_id
        .keys()
        .chain(actual_by_id.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (expected_by_id.get(id), actual_by_id.get(id)) {
            (Some(expected), Some(actual)) if performance_equivalent(expected, actual) => {}
            (Some(_), Some(_)) => mismatches.push(format!(
                "performance {id}: stored row differs from evidence"
            )),
            (Some(_), None) => mismatches.push(format!("performance {id}: evidence has no row")),
            (None, Some(_)) => mismatches.push(format!("performance {id}: row has no evidence")),
            (None, None) => {}
        }
    }
    mismatches
}

/// A stable content hash of the bounded candidate evidence. It is used in the
/// ADR/tests and available to future surfaces; it is not a security decision.
pub fn retirement_candidate_hash(candidate: &AgentRetirementCandidate) -> String {
    #[derive(Serialize)]
    struct HashInput<'a> {
        identity_id: &'a str,
        reasons: &'a [vp_protocol::types::AgentRetirementReason],
        performance: &'a AgentPerformance,
    }
    let encoded = serde_json::to_vec(&HashInput {
        identity_id: &candidate.identity_id,
        reasons: &candidate.reasons,
        performance: &candidate.performance,
    })
    .unwrap_or_default();
    let digest = Sha256::digest(encoded);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Convert one computed performance row into a deterministic retirement
/// candidate. This is a pure function; no store write and no model call.
pub fn retirement_candidate(
    identity: &AgentIdentity,
    performance: &AgentPerformance,
    policy: &AgentRetirementPolicy,
) -> Option<AgentRetirementCandidate> {
    if !policy.enabled || !policy.has_enabled_threshold() {
        return None;
    }
    if !matches!(identity.status.as_str(), "active" | "idle" | "paused") {
        return None;
    }
    if performance.completed_tasks < u64::from(policy.min_completed_tasks) {
        return None;
    }
    let mut reasons = Vec::new();
    if policy.max_consecutive_failures > 0
        && performance.consecutive_failures >= u64::from(policy.max_consecutive_failures)
    {
        reasons.push(vp_protocol::types::AgentRetirementReason {
            code: agent_retirement_reasons::CONSECUTIVE_FAILURES.to_string(),
            count: performance.consecutive_failures,
            threshold: policy.max_consecutive_failures,
        });
    }
    if policy.max_verifier_rejections > 0
        && performance.verifier_rejections >= u64::from(policy.max_verifier_rejections)
    {
        reasons.push(vp_protocol::types::AgentRetirementReason {
            code: agent_retirement_reasons::VERIFIER_REJECTIONS.to_string(),
            count: performance.verifier_rejections,
            threshold: policy.max_verifier_rejections,
        });
    }
    if policy.max_unsubstantiated_handoffs > 0
        && performance.unsubstantiated_handoffs >= u64::from(policy.max_unsubstantiated_handoffs)
    {
        reasons.push(vp_protocol::types::AgentRetirementReason {
            code: agent_retirement_reasons::UNSUBSTANTIATED_HANDOFFS.to_string(),
            count: performance.unsubstantiated_handoffs,
            threshold: policy.max_unsubstantiated_handoffs,
        });
    }
    if policy.max_false_consensus > 0
        && performance.false_consensus >= u64::from(policy.max_false_consensus)
    {
        reasons.push(vp_protocol::types::AgentRetirementReason {
            code: agent_retirement_reasons::FALSE_CONSENSUS.to_string(),
            count: performance.false_consensus,
            threshold: policy.max_false_consensus,
        });
    }
    if reasons.is_empty() {
        return None;
    }
    let severity = reasons
        .iter()
        .map(|reason| reason.count.saturating_sub(u64::from(reason.threshold)))
        .sum::<u64>()
        .saturating_add(reasons.len() as u64 * 1_000);
    let relevant = reasons
        .iter()
        .map(|reason| match reason.code.as_str() {
            agent_retirement_reasons::VERIFIER_REJECTIONS => perf_reasons::VERIFIER_REJECTION,
            agent_retirement_reasons::UNSUBSTANTIATED_HANDOFFS
            | agent_retirement_reasons::FALSE_CONSENSUS => perf_reasons::DIAGNOSIS,
            _ => perf_reasons::TASK_FAILED,
        })
        .collect::<BTreeSet<_>>();
    let mut evidence = performance
        .evidence
        .iter()
        .filter(|entry| {
            entry
                .reason
                .as_deref()
                .is_some_and(|reason| relevant.contains(reason))
        })
        .take(AGENT_PERFORMANCE_MAX_EVIDENCE)
        .cloned()
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        evidence = performance
            .evidence
            .iter()
            .take(AGENT_PERFORMANCE_MAX_EVIDENCE)
            .cloned()
            .collect();
    }
    Some(AgentRetirementCandidate {
        project_id: identity.project_id.clone(),
        identity_id: identity.id.clone(),
        role: identity.role.clone(),
        department: identity.department.clone(),
        component: identity.component.clone(),
        title: identity.title.clone(),
        status: identity.status.clone(),
        generation: identity.generation,
        severity,
        reasons,
        performance: performance.clone(),
        evidence,
    })
}

/// Evaluate every identity deterministically and return the bounded candidate
/// list. Sorting is by severity, then the most recent evidence, then identity
/// id; there is no model score or hidden ranking.
pub fn retirement_candidates(
    project_id: &str,
    identities: &[AgentIdentity],
    performances: &[AgentPerformance],
    policy: &AgentRetirementPolicy,
    last_proposal_at: &HashMap<String, OffsetDateTime>,
    now: OffsetDateTime,
    limit: Option<u32>,
) -> Vec<AgentRetirementCandidate> {
    if !policy.enabled || !policy.has_enabled_threshold() {
        return Vec::new();
    }
    let performance_by_id = performances
        .iter()
        .map(|row| (row.identity_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    let cooldown = Duration::seconds(policy.cooldown_secs as i64);
    let mut candidates = Vec::new();
    for identity in identities {
        let Some(performance) = performance_by_id.get(identity.id.as_str()) else {
            continue;
        };
        if let Some(last) = last_proposal_at.get(&identity.id)
            && now - *last < cooldown
        {
            continue;
        }
        if let Some(candidate) = retirement_candidate(identity, performance, policy) {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then_with(|| {
                let left_at = left
                    .performance
                    .last_active_at
                    .as_deref()
                    .unwrap_or_default();
                let right_at = right
                    .performance
                    .last_active_at
                    .as_deref()
                    .unwrap_or_default();
                right_at.cmp(left_at)
            })
            .then_with(|| left.identity_id.cmp(&right.identity_id))
    });
    let limit = limit
        .unwrap_or(AGENT_RETIREMENT_MAX_CANDIDATES)
        .clamp(1, AGENT_RETIREMENT_MAX_CANDIDATES) as usize;
    candidates.truncate(limit);
    let _ = project_id;
    candidates
}

/// Latest proposal timestamp per identity, used only for cooldown evaluation.
pub fn last_retirement_proposal_times(
    store: &Store,
    project_id: &str,
) -> Result<HashMap<String, OffsetDateTime>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT identity_id, MAX(proposed_at) FROM agent_retirement_proposals
         WHERE project_id = ?1 GROUP BY identity_id",
    )?;
    let rows = statement.query_map(params![project_id.trim()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut times = HashMap::new();
    for row in rows {
        let (identity_id, at) = row?;
        if let Some(parsed) = parse_time(&at) {
            times.insert(identity_id, parsed);
        }
    }
    Ok(times)
}

// ---------------------------------------------------------------------------
// Archive and rebuild surface
// ---------------------------------------------------------------------------

pub fn list_hire_requests_for_replay(
    store: &Store,
    project_id: &str,
) -> Result<Vec<AgentHireRequest>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "{HIRE_REQUEST_SELECT} WHERE project_id = ?1 ORDER BY requested_at ASC, id ASC LIMIT ?2"
    ))?;
    let rows = statement.query_map(
        params![project_id, AGENT_EMPLOYMENT_MAX_LIST as i64 * 20],
        map_hire_request,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn list_retirement_proposals_for_replay(
    store: &Store,
    project_id: &str,
) -> Result<Vec<AgentRetirementProposal>> {
    let conn = store.lock()?;
    let mut statement = conn.prepare(&format!(
        "{RETIREMENT_PROPOSAL_SELECT}
         WHERE project_id = ?1 ORDER BY proposed_at ASC, id ASC LIMIT ?2"
    ))?;
    let rows = statement.query_map(
        params![project_id, AGENT_EMPLOYMENT_MAX_LIST as i64 * 20],
        map_retirement_proposal,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn insert_hire_request_snapshot(store: &Store, request: &AgentHireRequest) -> Result<()> {
    let mut conn = store.lock()?;
    let tx = conn.transaction()?;
    insert_hire_request_tx(&tx, request)?;
    tx.commit()?;
    Ok(())
}

pub fn insert_retirement_proposal_snapshot(
    store: &Store,
    proposal: &AgentRetirementProposal,
) -> Result<()> {
    let mut conn = store.lock()?;
    let tx = conn.transaction()?;
    insert_retirement_proposal_tx(&tx, proposal)?;
    tx.commit()?;
    Ok(())
}

pub fn insert_agent_performance_snapshot(store: &Store, row: &AgentPerformance) -> Result<()> {
    let mut conn = store.lock()?;
    let tx = conn.transaction()?;
    insert_agent_performance_row_tx(&tx, row)?;
    tx.commit()?;
    Ok(())
}

pub fn insert_employment_event_snapshot(store: &Store, event: &AgentEmploymentEvent) -> Result<()> {
    let conn = store.lock()?;
    let payload = serde_json::to_string(&event.payload)?;
    conn.execute(
        "INSERT OR REPLACE INTO agent_employment_events
         (id, project_id, seq, identity_id, kind, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            event.id,
            event.project_id,
            event.seq as i64,
            event.identity_id,
            event.kind,
            payload,
            event.created_at
        ],
    )?;
    Ok(())
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

fn employment_projections_from_events(
    project_id: &str,
    events: &[AgentEmploymentEvent],
) -> (Vec<AgentHireRequest>, Vec<AgentRetirementProposal>) {
    let mut requests: BTreeMap<String, AgentHireRequest> = BTreeMap::new();
    let mut proposals: BTreeMap<String, AgentRetirementProposal> = BTreeMap::new();
    for event in events {
        match event.kind.as_str() {
            AGENT_HIRE_REQUESTED_EVENT | AGENT_HIRE_APPROVED_EVENT | AGENT_HIRE_REJECTED_EVENT => {
                if let Some(request) = event.payload.get("request")
                    && let Ok(mut request) =
                        serde_json::from_value::<AgentHireRequest>(request.clone())
                {
                    request.origin = "local".to_string();
                    requests.insert(request.id.clone(), request);
                }
            }
            AGENT_RETIREMENT_PROPOSED_EVENT
            | AGENT_RETIREMENT_APPROVED_EVENT
            | AGENT_RETIREMENT_REJECTED_EVENT => {
                if let Some(proposal) = event.payload.get("proposal")
                    && let Ok(mut proposal) =
                        serde_json::from_value::<AgentRetirementProposal>(proposal.clone())
                {
                    proposal.origin = "local".to_string();
                    proposals.insert(proposal.id.clone(), proposal);
                }
            }
            AGENT_RETIREMENT_EXECUTED_EVENT => {
                if let Some(proposal_id) = event.payload.get("proposal_id").and_then(Value::as_str)
                    && let Some(proposal) = proposals.get_mut(proposal_id)
                {
                    proposal.status = "executed".to_string();
                    proposal.executed_at = Some(event.created_at.clone());
                    if proposal.decided_at.is_none() {
                        proposal.decided_at = Some(event.created_at.clone());
                    }
                }
            }
            _ => {}
        }
    }
    let _ = project_id;
    (
        requests.into_values().collect(),
        proposals.into_values().collect(),
    )
}

/// Rebuild hire-request and proposal projections from the append-only
/// employment stream. Hire requests are reconstructed from the latest snapshot
/// carried by a requested/approved/rejected event; proposals from their
/// proposed/approved/rejected/executed events. Imported evidence rows are
/// preserved and never overwritten by a local rebuild.
pub fn rebuild_employment_projections(store: &Store, project_id: &str) -> Result<()> {
    let events = list_employment_events(store, project_id, None)?;
    let (requests, proposals) = employment_projections_from_events(project_id, &events);
    let mut conn = store.lock()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM agent_hire_requests WHERE project_id = ?1 AND origin = 'local'",
        params![project_id],
    )?;
    tx.execute(
        "DELETE FROM agent_retirement_proposals WHERE project_id = ?1 AND origin = 'local'",
        params![project_id],
    )?;
    for request in &requests {
        if get_hire_request_tx(&tx, &request.id)?.is_none() {
            insert_hire_request_tx(&tx, request)?;
        }
    }
    for proposal in &proposals {
        if get_retirement_proposal_tx(&tx, &proposal.id)?.is_none() {
            insert_retirement_proposal_tx(&tx, proposal)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn compare_hire_requests(
    expected: &[AgentHireRequest],
    actual: &[AgentHireRequest],
) -> Vec<String> {
    let expected_by_id = expected
        .iter()
        .map(|request| (request.id.as_str(), request))
        .collect::<BTreeMap<_, _>>();
    let comparable = actual
        .iter()
        .map(|request| {
            let mut request = request.clone();
            request.origin = "local".to_string();
            (request.id.clone(), request)
        })
        .collect::<Vec<_>>();
    let actual_by_id = comparable
        .iter()
        .map(|(id, request)| (id.as_str(), request))
        .collect::<BTreeMap<_, _>>();
    let mut mismatches = Vec::new();
    for id in expected_by_id
        .keys()
        .chain(actual_by_id.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (expected_by_id.get(id), actual_by_id.get(id)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(_), Some(_)) => mismatches.push(format!("hire request {id}: row differs")),
            (Some(_), None) => mismatches.push(format!("hire request {id}: event has no row")),
            (None, Some(_)) => mismatches.push(format!("hire request {id}: row has no event")),
            (None, None) => {}
        }
    }
    mismatches
}

fn compare_retirement_proposals(
    expected: &[AgentRetirementProposal],
    actual: &[AgentRetirementProposal],
) -> Vec<String> {
    let expected_by_id = expected
        .iter()
        .map(|proposal| (proposal.id.as_str(), proposal))
        .collect::<BTreeMap<_, _>>();
    let comparable = actual
        .iter()
        .map(|proposal| {
            let mut proposal = proposal.clone();
            proposal.origin = "local".to_string();
            (proposal.id.clone(), proposal)
        })
        .collect::<Vec<_>>();
    let actual_by_id = comparable
        .iter()
        .map(|(id, proposal)| (id.as_str(), proposal))
        .collect::<BTreeMap<_, _>>();
    let mut mismatches = Vec::new();
    for id in expected_by_id
        .keys()
        .chain(actual_by_id.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (expected_by_id.get(id), actual_by_id.get(id)) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(_), Some(_)) => mismatches.push(format!("retirement proposal {id}: row differs")),
            (Some(_), None) => {
                mismatches.push(format!("retirement proposal {id}: event has no row"))
            }
            (None, Some(_)) => {
                mismatches.push(format!("retirement proposal {id}: row has no event"))
            }
            (None, None) => {}
        }
    }
    mismatches
}

/// Read-only replay report for employment projections. Repair is explicit and
/// only rewrites local rows from the event stream.
pub fn employment_replay(
    store: &Store,
    project_id: &str,
    repair: bool,
) -> Result<vp_protocol::types::AgentEmploymentReplayReport> {
    let events = list_employment_events(store, project_id, None)?;
    let (expected_requests, expected_proposals) =
        employment_projections_from_events(project_id, &events);
    let mut requests = list_hire_requests_for_replay(store, project_id)?;
    let mut proposals = list_retirement_proposals_for_replay(store, project_id)?;
    let mut mismatches = compare_hire_requests(&expected_requests, &requests);
    mismatches.extend(compare_retirement_proposals(
        &expected_proposals,
        &proposals,
    ));
    let mut repaired = false;
    if repair && !mismatches.is_empty() {
        rebuild_employment_projections(store, project_id)?;
        requests = list_hire_requests_for_replay(store, project_id)?;
        proposals = list_retirement_proposals_for_replay(store, project_id)?;
        mismatches = compare_hire_requests(&expected_requests, &requests);
        mismatches.extend(compare_retirement_proposals(
            &expected_proposals,
            &proposals,
        ));
        repaired = true;
    }
    Ok(vp_protocol::types::AgentEmploymentReplayReport {
        project_id: project_id.to_string(),
        hire_requests: requests.len() as u64,
        proposals: proposals.len() as u64,
        events: events.len() as u64,
        consistent: mismatches.is_empty(),
        repaired,
        mismatches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AgentHireParams, AgentIdentityPolicy, AgentPromoteParams, AgentRetireParams,
        AgentRetirementPolicy, AgentTransferParams, AgentWorkspace, CreateAgentIdentityParams,
        Project, Session,
    };

    fn test_store() -> (TempDir, Store, TempDir) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        (data, store, repo)
    }

    fn project(store: &Store, repo: &TempDir) -> Project {
        store
            .add_project(repo.path().to_str().unwrap(), Some("s6"))
            .unwrap()
    }

    fn new_leader(store: &Store, project: &Project) -> AgentIdentity {
        store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "leader".into(),
                department: Some("executive".into()),
                component: None,
                title: Some("Leader".into()),
                profile: Some("generic".into()),
                model: None,
                parent_identity_id: None,
                policy: None,
            })
            .unwrap()
            .0
    }

    /// Resolve a legal Worker parent. S1 requires Worker→Manager, so a test
    /// that names the Leader gets (or creates) a deterministic platform
    /// Manager under that Leader.
    fn manager_parent(store: &Store, project: &Project, parent: &str) -> String {
        let identity = store.get_agent_identity(parent).unwrap().unwrap();
        if identity.role == "manager" {
            return identity.id;
        }
        let existing = store
            .list_agent_identities(&project.id, Some("manager"), None)
            .unwrap()
            .into_iter()
            .find(|manager| manager.parent_identity_id.as_deref() == Some(parent))
            .map(|manager| manager.id);
        if let Some(existing) = existing {
            return existing;
        }
        hire_agent(
            store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "manager".into(),
                department: Some("platform".into()),
                component: None,
                title: Some("Platform Manager".into()),
                profile: Some("generic".into()),
                model: None,
                parent_identity_id: Some(parent.into()),
                policy: None,
                approved: true,
                operator: Some("test".into()),
            },
        )
        .unwrap()
        .identity
        .unwrap()
        .id
    }

    fn new_worker(store: &Store, project: &Project, parent: &str, title: &str) -> AgentIdentity {
        let parent = manager_parent(store, project, parent);
        hire_agent(
            store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                component: Some("auth".into()),
                title: Some(title.into()),
                profile: Some("coding".into()),
                model: None,
                parent_identity_id: Some(parent),
                policy: None,
                approved: true,
                operator: Some("test".into()),
            },
        )
        .unwrap()
        .identity
        .unwrap()
    }

    fn manager(store: &Store, project: &Project, leader_id: &str, title: &str) -> AgentIdentity {
        hire_agent(
            store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "manager".into(),
                department: Some("platform".into()),
                component: None,
                title: Some(title.into()),
                profile: Some("generic".into()),
                model: None,
                parent_identity_id: Some(leader_id.into()),
                policy: None,
                approved: true,
                operator: Some("test".into()),
            },
        )
        .unwrap()
        .identity
        .unwrap()
    }

    fn enable_identities(store: &Store, project: &Project, max_managers: u32, max_workers: u32) {
        store
            .set_agent_identity_policy(
                &project.id,
                &AgentIdentityPolicy {
                    enabled: true,
                    max_managers,
                    max_workers,
                    hire_requires_approval: true,
                    retirement_requires_approval: true,
                },
            )
            .unwrap();
    }

    fn assign(store: &Store, session: &Session, identity: &AgentIdentity) {
        store
            .create_session_agent_assignment(
                &session.id,
                &identity.id,
                Some(&identity.role),
                Some(600),
                Some("test"),
            )
            .unwrap();
    }

    fn active_workspace(
        store: &Store,
        project: &Project,
        identity: &AgentIdentity,
    ) -> AgentWorkspace {
        let workspace = AgentWorkspace {
            id: Uuid::now_v7().to_string(),
            project_id: project.id.clone(),
            identity_id: identity.id.clone(),
            component: identity.component.clone().unwrap_or_else(|| "auth".into()),
            branch: vp_protocol::types::component_branch_name(
                &identity.id,
                identity.component.as_deref().unwrap_or("auth"),
            ),
            base_commit: "a".repeat(40),
            worktree_path: format!("/tmp/s6-{}", identity.id),
            status: "active".into(),
            created_at: super::super::now_rfc3339(),
            updated_at: super::super::now_rfc3339(),
            last_session_id: None,
            lease_active: false,
            lease_expires_at: None,
            origin: "local".into(),
        };
        store.insert_agent_workspace_snapshot(&workspace).unwrap();
        workspace
    }

    fn retirement_policy() -> AgentRetirementPolicy {
        AgentRetirementPolicy {
            enabled: true,
            lookback_days: 30,
            min_completed_tasks: 1,
            max_consecutive_failures: 2,
            max_verifier_rejections: 1,
            max_unsubstantiated_handoffs: 1,
            max_false_consensus: 1,
            cooldown_secs: 60,
            require_approval: true,
        }
    }

    #[test]
    fn hire_requires_approval_and_never_creates_an_identity_implicitly() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        let leader = new_leader(&store, &project);
        let manager = manager(&store, &project, &leader.id, "Platform Manager");
        let pending = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                component: Some("auth".into()),
                title: Some("Auth Worker".into()),
                profile: Some("coding".into()),
                model: None,
                parent_identity_id: Some(manager.id.clone()),
                policy: None,
                approved: false,
                operator: Some("ops".into()),
            },
        )
        .unwrap();
        assert_eq!(pending.state, "pending");
        assert!(pending.identity.is_none());
        assert!(
            store
                .get_agent_identity(&pending.request.identity_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .list_agent_identities(&project.id, None, None)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(pending.request.approval_risk, "host_state");
        let events = list_employment_events(&store, &project.id, None).unwrap();
        assert_eq!(events.last().unwrap().kind, AGENT_HIRE_REQUESTED_EVENT);

        let approved = approve_agent_hire(
            &store,
            &vp_protocol::types::AgentHireApproveParams {
                request_id: pending.request.id.clone(),
                operator: Some("ops".into()),
                approval_risk: Some("host_state".into()),
            },
        )
        .unwrap();
        assert_eq!(approved.state, "approved");
        assert_eq!(
            approved.identity.as_ref().unwrap().id,
            pending.request.identity_id
        );
        assert_eq!(approved.identity.as_ref().unwrap().status, "active");
        let second = approve_agent_hire(
            &store,
            &vp_protocol::types::AgentHireApproveParams {
                request_id: pending.request.id,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&second),
            agent_identity_codes::HIRE_ALREADY_DECIDED
        );
    }

    #[test]
    fn hire_rejection_is_terminal_and_creates_no_identity() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        let leader = new_leader(&store, &project);
        let manager = manager(&store, &project, &leader.id, "Platform Manager");
        let pending = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                component: Some("api".into()),
                title: None,
                profile: Some("coding".into()),
                model: None,
                parent_identity_id: Some(manager.id),
                policy: None,
                approved: false,
                operator: None,
            },
        )
        .unwrap();
        let rejected = reject_agent_hire(
            &store,
            &vp_protocol::types::AgentHireRejectParams {
                request_id: pending.request.id.clone(),
                reason: Some("headcount frozen".into()),
                operator: Some("ops".into()),
            },
        )
        .unwrap();
        assert_eq!(rejected.state, "rejected");
        assert!(
            store
                .get_agent_identity(&pending.request.identity_id)
                .unwrap()
                .is_none()
        );
        let approve = approve_agent_hire(
            &store,
            &vp_protocol::types::AgentHireApproveParams {
                request_id: pending.request.id,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&approve),
            agent_identity_codes::HIRE_ALREADY_DECIDED
        );
    }

    #[test]
    fn hire_caps_and_hierarchy_apply_even_when_the_policy_is_disabled() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        let leader = new_leader(&store, &project);
        // Default policy is disabled but still carries the bounded caps.
        let store_before = store.get_agent_identity_policy(&project.id).unwrap();
        assert!(!store_before.enabled);
        store
            .set_agent_identity_policy(
                &project.id,
                &AgentIdentityPolicy {
                    enabled: false,
                    max_managers: 1,
                    max_workers: 0,
                    hire_requires_approval: false,
                    retirement_requires_approval: true,
                },
            )
            .unwrap();
        let manager = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "manager".into(),
                department: Some("platform".into()),
                title: Some("Platform Manager".into()),
                parent_identity_id: Some(leader.id.clone()),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(manager.state, "approved");
        let second = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "manager".into(),
                department: Some("platform".into()),
                parent_identity_id: Some(leader.id),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&second),
            agent_identity_codes::IDENTITY_CAP_EXCEEDED
        );
        let worker = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                parent_identity_id: manager
                    .identity
                    .as_ref()
                    .map(|identity| identity.id.clone()),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&worker),
            agent_identity_codes::IDENTITY_CAP_EXCEEDED
        );
        let leader_hire = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "leader".into(),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error_code(&leader_hire), agent_identity_codes::HIRE_INVALID);
        let wrong_parent = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                parent_identity_id: Some(leader_hire_id(&store, &project)),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&wrong_parent),
            agent_identity_codes::IDENTITY_INVALID
        );
    }

    fn leader_hire_id(store: &Store, project: &Project) -> String {
        store
            .list_agent_identities(&project.id, Some("leader"), None)
            .unwrap()
            .first()
            .map(|identity| identity.id.clone())
            .unwrap_or_default()
    }

    #[test]
    fn one_live_leader_is_enforced_after_retirement() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        let first = new_leader(&store, &project);
        let duplicate = store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "leader".into(),
                title: Some("Second Leader".into()),
                profile: Some("generic".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(
            error_code(&duplicate),
            agent_identity_codes::IDENTITY_CONFLICT
        );
        let retired = retire_agent(
            &store,
            &AgentRetireParams {
                identity_id: first.id.clone(),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(retired.changed);
        assert_eq!(retired.identity.status, "retired");
        let replacement = store
            .create_agent_identity(&CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: "leader".into(),
                title: Some("Replacement Leader".into()),
                profile: Some("generic".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(replacement.0.status, "active");
        assert_ne!(replacement.0.id, first.id);
    }

    #[test]
    fn promotion_and_transfer_are_generation_bumped_explicit_and_force_released() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        let leader = new_leader(&store, &project);
        let manager = manager(&store, &project, &leader.id, "Platform Manager");
        let worker = new_worker(&store, &project, &manager.id, "Auth Worker");
        let session = store.create_session(&project.id, "promotion").unwrap();
        assign(&store, &session, &worker);
        let workspace = active_workspace(&store, &project, &worker);

        let without_approval = promote_agent(
            &store,
            &AgentPromoteParams {
                identity_id: worker.id.clone(),
                target_role: "manager".into(),
                parent_identity_id: Some(leader.id.clone()),
                approved: false,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&without_approval),
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED
        );
        let without_force = promote_agent(
            &store,
            &AgentPromoteParams {
                identity_id: worker.id.clone(),
                target_role: "manager".into(),
                parent_identity_id: Some(leader.id.clone()),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&without_force),
            agent_identity_codes::IDENTITY_BUSY
        );

        let promoted = promote_agent(
            &store,
            &AgentPromoteParams {
                identity_id: worker.id.clone(),
                target_role: "manager".into(),
                parent_identity_id: Some(leader.id.clone()),
                title: Some("Auth Manager".into()),
                approved: true,
                force: true,
                operator: Some("ops".into()),
            },
        )
        .unwrap();
        assert_eq!(promoted.identity.role, "manager");
        assert_eq!(promoted.identity.generation, worker.generation + 1);
        assert_eq!(promoted.identity.title.as_deref(), Some("Auth Manager"));
        assert_eq!(promoted.released_assignments, 1);
        assert_eq!(promoted.archived_workspaces, 1);
        let archived = store.get_agent_workspace(&workspace.id).unwrap().unwrap();
        assert_eq!(archived.status, "archived");
        assert_eq!(archived.branch, workspace.branch);

        let transferred = transfer_agent(
            &store,
            &AgentTransferParams {
                identity_id: promoted.identity.id.clone(),
                department: Some(Some("identity".into())),
                component: Some(Some("auth".into())),
                title: None,
                parent_identity_id: None,
                approved: true,
                force: false,
                operator: Some("ops".into()),
            },
        )
        .unwrap();
        assert_eq!(transferred.identity.department.as_deref(), Some("identity"));
        assert_eq!(
            transferred.identity.generation,
            promoted.identity.generation + 1
        );
        let too_many = promote_agent(
            &store,
            &AgentPromoteParams {
                identity_id: manager.id.clone(),
                target_role: "leader".into(),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&too_many),
            agent_identity_codes::IDENTITY_CONFLICT
        );
        let invalid = promote_agent(
            &store,
            &AgentPromoteParams {
                identity_id: leader.id,
                target_role: "manager".into(),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&invalid),
            agent_identity_codes::PROMOTION_INVALID
        );
    }

    #[test]
    fn transfer_refuses_an_active_assignment_without_force() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        let leader = new_leader(&store, &project);
        let worker = new_worker(&store, &project, &leader.id, "Auth Worker");
        let session = store.create_session(&project.id, "transfer").unwrap();
        assign(&store, &session, &worker);
        let refused = transfer_agent(
            &store,
            &AgentTransferParams {
                identity_id: worker.id.clone(),
                department: Some(Some("identity".into())),
                component: None,
                title: None,
                parent_identity_id: None,
                approved: true,
                force: false,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&refused),
            agent_identity_codes::TRANSFER_CONFLICT
        );
        let forced = transfer_agent(
            &store,
            &AgentTransferParams {
                identity_id: worker.id.clone(),
                department: Some(Some("identity".into())),
                component: None,
                title: None,
                parent_identity_id: None,
                approved: true,
                force: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(forced.released_assignments, 1);
        let assignment = store
            .get_session_agent_assignment(&session.id, &worker.id)
            .unwrap()
            .unwrap();
        assert_eq!(assignment.status, "released");
    }

    fn append_task(
        store: &Store,
        session: &Session,
        identity: &AgentIdentity,
        task_id: &str,
        status: &str,
    ) {
        store
            .append_event(
                &session.id,
                "plan_task.finished",
                serde_json::json!({
                    "task_id": task_id,
                    "task_status": status,
                    "agent_id": format!("agent-{task_id}"),
                    "agent_role": identity.role,
                    "agent_name": identity.title,
                    "identity_id": identity.id,
                }),
            )
            .unwrap();
    }

    fn append_attribution(
        store: &Store,
        session: &Session,
        identity: &AgentIdentity,
        reason: &str,
        id: &str,
    ) {
        store
            .append_event(
                &session.id,
                "failure.attribution",
                serde_json::json!({
                    "attribution": {
                        "schema_version": 1,
                        "attribution_id": id,
                        "session_id": session.id,
                        "status": "failed",
                        "reason_code": reason,
                        "responsible_agents": [{"identity_id": identity.id, "agent_name": "Worker"}],
                        "findings": [],
                        "advisory": true,
                        "created_at": vp_protocol::now_rfc3339(),
                    }
                }),
            )
            .unwrap();
    }

    #[test]
    fn performance_counts_tasks_usage_verifier_and_diagnosis_evidence() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        let leader = new_leader(&store, &project);
        let worker = new_worker(&store, &project, &leader.id, "Auth Worker");
        let session = store.create_session(&project.id, "performance").unwrap();
        assign(&store, &session, &worker);
        append_task(&store, &session, &worker, "t1", "completed");
        append_task(&store, &session, &worker, "t2", "failed");
        append_task(&store, &session, &worker, "t3", "failed");
        store
            .append_event(
                &session.id,
                "plan_task.retry_scheduled",
                serde_json::json!({
                    "task_id": "t3",
                    "agent_id": "agent-t3",
                    "agent_role": "worker",
                    "identity_id": worker.id,
                }),
            )
            .unwrap();
        append_attribution(
            &store,
            &session,
            &worker,
            agent_retirement_reasons::FALSE_CONSENSUS,
            "att-1",
        );
        append_attribution(
            &store,
            &session,
            &worker,
            agent_retirement_reasons::UNSUBSTANTIATED_HANDOFFS,
            "att-2",
        );
        store
            .add_usage_entry_with_events(
                &session.id,
                "agent-t1",
                "Auth Worker",
                "worker",
                Some("t1"),
                Some("platform"),
                "model-x",
                1_000,
                200,
                500,
                0,
                true,
                0.25,
            )
            .unwrap();
        let (entry, _) = store
            .enqueue_merge_with_event(
                &session.id,
                "agent-t2",
                "Auth Worker",
                Some("t2"),
                "worker/t2",
                &"b".repeat(40),
                &"a".repeat(40),
                None,
            )
            .unwrap();
        store
            .set_merge_queue_verification_with_event(&entry.id, Some(false), Some("rejected"), None)
            .unwrap();
        let now = OffsetDateTime::now_utc();
        let rows = compute_agent_performance(&store, &project.id, 30, now).unwrap();
        let row = rows
            .iter()
            .find(|row| row.identity_id == worker.id)
            .unwrap();
        assert_eq!(row.completed_tasks, 1);
        assert_eq!(row.failed_tasks, 2);
        assert_eq!(row.consecutive_failures, 2);
        assert_eq!(row.retries, 1);
        assert_eq!(row.verifier_rejections, 1);
        assert_eq!(row.false_consensus, 1);
        assert_eq!(row.unsubstantiated_handoffs, 1);
        assert_eq!(row.input_tokens, 1_000);
        assert_eq!(row.output_tokens, 200);
        assert_eq!(row.cache_read_tokens, 500);
        assert_eq!(row.estimated_usage_calls, 1);
        assert!((row.cost_usd - 0.25).abs() < f64::EPSILON);
        assert!(row.evidence.len() <= AGENT_PERFORMANCE_MAX_EVIDENCE);
        assert!(!row.evidence.iter().any(|evidence| {
            evidence.reference.contains("goal") || evidence.reference.contains("prompt")
        }));
        let reason = row
            .reason_counts
            .iter()
            .find(|count| count.code == agent_retirement_reasons::FALSE_CONSENSUS)
            .unwrap();
        assert_eq!(reason.count, 1);
        let again = compute_agent_performance(&store, &project.id, 30, now).unwrap();
        assert_eq!(rows, again);
    }

    #[test]
    fn retirement_proposes_but_never_fires_and_requires_approval() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        set_retirement_policy(&store, &project.id, &retirement_policy()).unwrap();
        let leader = new_leader(&store, &project);
        let worker = new_worker(&store, &project, &leader.id, "Auth Worker");
        let successor = new_worker(&store, &project, &leader.id, "Auth Worker II");
        let session = store.create_session(&project.id, "retire").unwrap();
        assign(&store, &session, &worker);
        let workspace = active_workspace(&store, &project, &worker);
        append_task(&store, &session, &worker, "t1", "completed");
        append_task(&store, &session, &worker, "t2", "failed");
        append_task(&store, &session, &worker, "t3", "failed");
        let now = OffsetDateTime::now_utc();
        let rows = compute_agent_performance(&store, &project.id, 30, now).unwrap();
        let performance = rows
            .iter()
            .find(|row| row.identity_id == worker.id)
            .unwrap();
        let candidate = retirement_candidate(&worker, performance, &retirement_policy()).unwrap();
        assert!(
            candidate
                .reasons
                .iter()
                .any(|reason| { reason.code == agent_retirement_reasons::CONSECUTIVE_FAILURES })
        );
        assert!(candidate.evidence.len() <= vp_protocol::types::AGENT_PERFORMANCE_MAX_EVIDENCE);
        let (proposal, event, already) =
            create_retirement_proposal(&store, &candidate, Some("ops")).unwrap();
        assert!(!already);
        assert!(event.is_some());
        assert_eq!(proposal.status, "pending");
        assert_eq!(
            store
                .get_agent_identity(&worker.id)
                .unwrap()
                .unwrap()
                .status,
            "active"
        );
        let unapproved = retire_agent(
            &store,
            &AgentRetireParams {
                identity_id: worker.id.clone(),
                successor_id: Some(successor.id.clone()),
                proposal_id: Some(proposal.id.clone()),
                approved: false,
                force: false,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&unapproved),
            agent_identity_codes::RETIREMENT_APPROVAL_REQUIRED
        );
        let approved = approve_retirement_proposal(
            &store,
            &vp_protocol::types::AgentRetirementProposalParams {
                proposal_id: proposal.id.clone(),
                operator: Some("ops".into()),
            },
        )
        .unwrap();
        assert_eq!(approved.proposal.status, "approved");
        let retired = retire_agent(
            &store,
            &AgentRetireParams {
                identity_id: worker.id.clone(),
                successor_id: Some(successor.id.clone()),
                proposal_id: Some(proposal.id.clone()),
                approved: false,
                force: true,
                reason: Some("threshold".into()),
                operator: Some("ops".into()),
            },
        )
        .unwrap();
        assert!(retired.changed);
        assert_eq!(retired.identity.status, "retired");
        assert_eq!(
            retired.identity.successor_id.as_deref(),
            Some(successor.id.as_str())
        );
        assert_eq!(retired.released_assignments, 1);
        assert_eq!(retired.archived_workspaces, 1);
        assert_eq!(retired.proposal_id.as_deref(), Some(proposal.id.as_str()));
        let archived = store.get_agent_workspace(&workspace.id).unwrap().unwrap();
        assert_eq!(archived.status, "archived");
        assert_eq!(archived.branch, workspace.branch);
        let executed = list_retirement_proposals(
            &store,
            &vp_protocol::types::AgentRetirementProposalListParams {
                project_id: project.id.clone(),
                ..Default::default()
            },
        )
        .unwrap()
        .proposals;
        assert_eq!(executed[0].status, "executed");
        let assignment = store
            .get_session_agent_assignment(&session.id, &worker.id)
            .unwrap()
            .unwrap();
        assert_eq!(assignment.status, "released");
        let reassign = store
            .create_session_agent_assignment(&session.id, &worker.id, None, Some(600), Some("test"))
            .unwrap_err();
        assert_eq!(
            error_code(&reassign),
            vp_protocol::types::agent_coordination_codes::ASSIGNMENT_IDENTITY_RETIRED
        );
        let memory_count =
            crate::store::identity_memory::list_agent_memories_for_replay(&store, &project.id)
                .unwrap()
                .len();
        assert_eq!(memory_count, 0);
        // The identity row and every lifecycle/employment event are retained.
        assert_eq!(
            store
                .get_agent_identity(&worker.id)
                .unwrap()
                .unwrap()
                .status,
            "retired"
        );
        assert!(
            !store
                .list_agent_identity_events(&worker.id, None)
                .unwrap()
                .is_empty()
        );
        assert!(
            !list_employment_events(&store, &project.id, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn retirement_successor_must_be_compatible_and_same_project() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        let leader = new_leader(&store, &project);
        let worker = new_worker(&store, &project, &leader.id, "Auth Worker");
        let manager = manager(&store, &project, &leader.id, "Platform Manager");
        let incompatible = retire_agent(
            &store,
            &AgentRetireParams {
                identity_id: worker.id.clone(),
                successor_id: Some(manager.id),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&incompatible),
            agent_identity_codes::SUCCESSOR_INVALID
        );
        let other_repo = TempDir::new().unwrap();
        let other_project = store
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let other_leader = new_leader(&store, &other_project);
        let other_worker = new_worker(&store, &other_project, &other_leader.id, "Other Worker");
        let cross_project = retire_agent(
            &store,
            &AgentRetireParams {
                identity_id: worker.id,
                successor_id: Some(other_worker.id),
                approved: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&cross_project),
            agent_identity_codes::IDENTITY_PROJECT_MISMATCH
        );
        let _ = manager;
    }

    #[test]
    fn retirement_policy_bounds_and_disabled_candidates_are_deterministic() {
        let policy = AgentRetirementPolicy::default();
        assert!(!policy.enabled);
        assert!(policy.has_enabled_threshold());
        assert!(policy.normalized().is_ok());
        let invalid = AgentRetirementPolicy {
            lookback_days: 0,
            ..AgentRetirementPolicy::default()
        };
        assert!(invalid.normalized().is_err());
        let invalid = AgentRetirementPolicy {
            cooldown_secs: 0,
            ..AgentRetirementPolicy::default()
        };
        assert!(invalid.normalized().is_err());
        let invalid = AgentRetirementPolicy {
            max_consecutive_failures: 101,
            ..AgentRetirementPolicy::default()
        };
        assert!(invalid.normalized().is_err());
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        let leader = new_leader(&store, &project);
        let worker = new_worker(&store, &project, &leader.id, "Auth Worker");
        let session = store.create_session(&project.id, "disabled").unwrap();
        assign(&store, &session, &worker);
        append_task(&store, &session, &worker, "t1", "completed");
        append_task(&store, &session, &worker, "t2", "failed");
        let rows =
            compute_agent_performance(&store, &project.id, 30, OffsetDateTime::now_utc()).unwrap();
        let performance = rows
            .iter()
            .find(|row| row.identity_id == worker.id)
            .unwrap();
        assert!(
            retirement_candidate(&worker, performance, &AgentRetirementPolicy::default()).is_none()
        );
        let candidates = retirement_candidates(
            &project.id,
            std::slice::from_ref(&worker),
            &rows,
            &AgentRetirementPolicy::default(),
            &HashMap::new(),
            OffsetDateTime::now_utc(),
            None,
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn performance_replay_detects_drift_and_repairs_explicitly() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        let leader = new_leader(&store, &project);
        let worker = new_worker(&store, &project, &leader.id, "Auth Worker");
        let session = store.create_session(&project.id, "replay").unwrap();
        assign(&store, &session, &worker);
        append_task(&store, &session, &worker, "t1", "completed");
        let now = OffsetDateTime::now_utc();
        let rows = compute_agent_performance(&store, &project.id, 30, now).unwrap();
        replace_agent_performance_projection(&store, &project.id, &rows).unwrap();
        let report = performance_replay(&store, &project.id, false, now).unwrap();
        assert!(report.consistent);
        let mut drifted = rows.clone();
        drifted[0].completed_tasks = 99;
        replace_agent_performance_projection(&store, &project.id, &drifted).unwrap();
        let report = performance_replay(&store, &project.id, false, now).unwrap();
        assert!(!report.consistent);
        assert_eq!(report.rows, rows.len() as u64);
        let repaired = performance_replay(&store, &project.id, true, now).unwrap();
        assert!(repaired.repaired);
        assert!(repaired.consistent);
        let stored = list_agent_performance(&store, &project.id, None).unwrap();
        assert_eq!(
            stored
                .iter()
                .find(|row| row.identity_id == worker.id)
                .unwrap()
                .completed_tasks,
            1
        );
    }

    #[test]
    fn employment_replay_rebuilds_requests_and_proposals() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        let leader = new_leader(&store, &project);
        let manager = manager(&store, &project, &leader.id, "Platform Manager");
        let pending = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                component: Some("auth".into()),
                parent_identity_id: Some(manager.id),
                approved: false,
                ..Default::default()
            },
        )
        .unwrap();
        set_retirement_policy(&store, &project.id, &retirement_policy()).unwrap();
        let session = store.create_session(&project.id, "proposal").unwrap();
        assign(&store, &session, &leader);
        store
            .append_event(
                &session.id,
                "plan_task.finished",
                serde_json::json!({
                    "task_id": "t1",
                    "task_status": "completed",
                    "agent_role": "leader",
                    "identity_id": leader.id,
                }),
            )
            .unwrap();
        store
            .append_event(
                &session.id,
                "failure.attribution",
                serde_json::json!({
                    "attribution": {
                        "attribution_id": "att-replay",
                        "reason_code": agent_retirement_reasons::FALSE_CONSENSUS,
                        "responsible_agents": [{"identity_id": leader.id}],
                        "findings": [],
                        "advisory": true
                    }
                }),
            )
            .unwrap();
        let rows =
            compute_agent_performance(&store, &project.id, 30, OffsetDateTime::now_utc()).unwrap();
        let candidate = retirement_candidate(
            &leader,
            rows.iter()
                .find(|row| row.identity_id == leader.id)
                .unwrap(),
            &retirement_policy(),
        )
        .unwrap();
        let (proposal, _, _) = create_retirement_proposal(&store, &candidate, Some("ops")).unwrap();
        let clean = employment_replay(&store, &project.id, false).unwrap();
        assert!(clean.consistent);
        {
            let conn = store.lock().unwrap();
            conn.execute(
                "DELETE FROM agent_hire_requests WHERE project_id = ?1 AND origin = 'local'",
                rusqlite::params![project.id],
            )
            .unwrap();
        }
        let report = employment_replay(&store, &project.id, true).unwrap();
        assert!(report.consistent);
        assert_eq!(report.hire_requests, 2);
        assert_eq!(report.proposals, 1);
        assert!(report.repaired);
        let requests = list_agent_hire_requests(
            &store,
            &AgentHireListParams {
                project_id: project.id.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            requests
                .requests
                .iter()
                .any(|request| request.id == pending.request.id)
        );
        let proposals = list_retirement_proposals(
            &store,
            &vp_protocol::types::AgentRetirementProposalListParams {
                project_id: project.id.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(proposals.proposals[0].id, proposal.id);
    }

    #[allow(clippy::too_many_arguments)]
    fn minimal_bundle(
        project: Project,
        session: Session,
        identities: Vec<AgentIdentity>,
        identity_events: Vec<vp_protocol::types::AgentIdentityEvent>,
        performance: Vec<AgentPerformance>,
        proposals: Vec<AgentRetirementProposal>,
        hire_requests: Vec<AgentHireRequest>,
        employment_events: Vec<AgentEmploymentEvent>,
    ) -> vp_protocol::types::SessionBundle {
        vp_protocol::types::SessionBundle {
            version: 1,
            exported_at: vp_protocol::now_rfc3339(),
            project,
            session,
            events: Vec::new(),
            agents: Vec::new(),
            messages: Vec::new(),
            team_plan: None,
            todos: Vec::new(),
            approvals: Vec::new(),
            merge_queue: Vec::new(),
            agent_messages: Vec::new(),
            artifacts: Vec::new(),
            usage: Vec::new(),
            memories: Vec::new(),
            memory_conflicts: Vec::new(),
            questions: Vec::new(),
            tasks: Vec::new(),
            steering: Vec::new(),
            continuations: Vec::new(),
            gates: Vec::new(),
            policy_snapshots: Vec::new(),
            recoveries: Vec::new(),
            programs: Vec::new(),
            program_milestones: Vec::new(),
            program_events: Vec::new(),
            session_runs: Vec::new(),
            run_steps: Vec::new(),
            run_checkpoints: Vec::new(),
            context_checkpoints: Vec::new(),
            supervised_processes: Vec::new(),
            test_results: Vec::new(),
            instruction_blobs: Vec::new(),
            instructions: Vec::new(),
            prompt_prefix_observations: Vec::new(),
            effects: Vec::new(),
            agent_identities: identities,
            agent_identity_events: identity_events,
            session_agent_assignments: Vec::new(),
            agent_inbox: Vec::new(),
            agent_coordination_events: Vec::new(),
            agent_workspaces: Vec::new(),
            session_worker_workspaces: Vec::new(),
            agent_workspace_events: Vec::new(),
            agent_memories: Vec::new(),
            agent_memory_events: Vec::new(),
            agent_performance: performance,
            agent_retirement_proposals: proposals,
            agent_employment_events: employment_events,
            agent_hire_requests: hire_requests,
        }
    }

    #[test]
    fn bundle_round_trip_imports_s6_evidence_as_read_only() {
        let (_data, store, repo) = test_store();
        let project = project(&store, &repo);
        enable_identities(&store, &project, 2, 2);
        set_retirement_policy(&store, &project.id, &retirement_policy()).unwrap();
        let leader = new_leader(&store, &project);
        let manager = manager(&store, &project, &leader.id, "Platform Manager");
        let session = store.create_session(&project.id, "bundle").unwrap();
        assign(&store, &session, &leader);
        append_task(&store, &session, &leader, "t1", "completed");
        append_task(&store, &session, &leader, "t2", "failed");
        append_task(&store, &session, &leader, "t3", "failed");
        let pending = hire_agent(
            &store,
            &AgentHireParams {
                project_id: project.id.clone(),
                role: "worker".into(),
                department: Some("platform".into()),
                component: Some("auth".into()),
                parent_identity_id: Some(manager.id.clone()),
                approved: false,
                ..Default::default()
            },
        )
        .unwrap();
        let rows =
            compute_agent_performance(&store, &project.id, 30, OffsetDateTime::now_utc()).unwrap();
        let candidate = retirement_candidate(
            &leader,
            rows.iter()
                .find(|row| row.identity_id == leader.id)
                .unwrap(),
            &retirement_policy(),
        )
        .unwrap();
        let (proposal, _, _) = create_retirement_proposal(&store, &candidate, Some("ops")).unwrap();
        let identities = store
            .list_agent_identities(&project.id, None, None)
            .unwrap();
        let identity_events = store
            .list_agent_identity_events_for_project(&project.id, None)
            .unwrap();
        let proposals = list_retirement_proposals_for_replay(&store, &project.id).unwrap();
        let hire_requests = list_hire_requests_for_replay(&store, &project.id).unwrap();
        let employment_events = list_employment_events(&store, &project.id, None).unwrap();
        let employment_event_count = employment_events.len();
        let bundle = minimal_bundle(
            project.clone(),
            session,
            identities,
            identity_events,
            rows,
            proposals,
            hire_requests,
            employment_events,
        );
        crate::store::validate_session_bundle(&bundle).unwrap();
        let other_data = TempDir::new().unwrap();
        let other = Store::open(other_data.path()).unwrap();
        other.import_session_bundle(&bundle).unwrap();
        let imported = list_agent_performance(&other, &project.id, None).unwrap();
        assert_eq!(imported.len(), 2);
        let imported_proposals = list_retirement_proposals_for_replay(&other, &project.id).unwrap();
        assert_eq!(imported_proposals.len(), 1);
        assert_eq!(imported_proposals[0].origin, "imported");
        assert_eq!(imported_proposals[0].id, proposal.id);
        let imported_requests = list_hire_requests_for_replay(&other, &project.id).unwrap();
        assert!(
            imported_requests.iter().any(|request| {
                request.id == pending.request.id && request.origin == "imported"
            })
        );
        let approve = approve_agent_hire(
            &other,
            &vp_protocol::types::AgentHireApproveParams {
                request_id: pending.request.id,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&approve),
            agent_identity_codes::IDENTITY_IMPORTED_READONLY
        );
        let decide = reject_retirement_proposal(
            &other,
            &vp_protocol::types::AgentRetirementRejectParams {
                proposal_id: proposal.id.clone(),
                reason: Some("archive evidence".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error_code(&decide),
            agent_identity_codes::IDENTITY_IMPORTED_READONLY
        );
        // Imported event ids keep the append-only stream byte-stable.
        assert_eq!(
            list_employment_events(&other, &project.id, None)
                .unwrap()
                .len(),
            employment_event_count
        );
    }

    fn error_code(error: &anyhow::Error) -> &'static str {
        error
            .downcast_ref::<AgentIdentityError>()
            .map(|typed| typed.code)
            .or_else(|| {
                error
                    .downcast_ref::<vp_protocol::types::AgentCoordinationError>()
                    .map(|typed| typed.code)
            })
            .unwrap_or("UNKNOWN")
    }
}

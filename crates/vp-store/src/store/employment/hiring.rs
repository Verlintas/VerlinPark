use super::*;

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
    let now = super::super::now_rfc3339();
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
    let now = super::super::now_rfc3339();
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
    request.decided_at = Some(super::super::now_rfc3339());
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
           AND (?3 IS NULL OR identity_id = ?3)
         ORDER BY requested_at DESC, id DESC LIMIT ?4"
    ))?;
    let identity_id = params
        .identity_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requests = statement
        .query_map(
            params![
                params.project_id.trim(),
                params.status.as_deref(),
                identity_id,
                limit
            ],
            map_hire_request,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentHireListResult { requests })
}

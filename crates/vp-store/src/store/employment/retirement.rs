use super::*;

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

pub(super) fn map_retirement_proposal(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AgentRetirementProposal> {
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

pub(super) const RETIREMENT_PROPOSAL_SELECT: &str = "SELECT id, project_id, identity_id, status,
    reasons_json, evidence_json, performance_json, reason_code, proposed_at, proposed_by,
    decided_at, decided_by, decision_reason, successor_id, executed_at, origin
    FROM agent_retirement_proposals";

pub(super) fn get_retirement_proposal_tx(
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

pub(super) fn insert_retirement_proposal_tx(
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
    let now = super::super::now_rfc3339();
    let mut released_assignments = 0;
    let mut archived_workspaces = 0;
    if params.force {
        let released = super::super::coordination::release_identity_assignments_tx(
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
    proposal.decided_at = Some(super::super::now_rfc3339());
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
            super::super::now_rfc3339()
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
    let limit = limit
        .map(|value| value.clamp(1, 100_000) as i64)
        .unwrap_or(-1);
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
            payload: serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            created_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

use super::*;

fn employment_projections_from_events(
    project_id: &str,
    events: &[AgentEmploymentEvent],
) -> Result<(Vec<AgentHireRequest>, Vec<AgentRetirementProposal>)> {
    let mut requests: BTreeMap<String, AgentHireRequest> = BTreeMap::new();
    let mut proposals: BTreeMap<String, AgentRetirementProposal> = BTreeMap::new();
    for event in events {
        if event.project_id != project_id {
            return Err(employment_error(
                agent_identity_codes::IDENTITY_PROJECT_MISMATCH,
                format!(
                    "employment event {} belongs to project {}, not {project_id}",
                    event.id, event.project_id
                ),
            ));
        }
        match event.kind.as_str() {
            AGENT_HIRE_REQUESTED_EVENT | AGENT_HIRE_APPROVED_EVENT | AGENT_HIRE_REJECTED_EVENT => {
                if let Some(request) = event.payload.get("request") {
                    let mut request = serde_json::from_value::<AgentHireRequest>(request.clone())
                        .map_err(|error| {
                        employment_error(
                            agent_identity_codes::IDENTITY_INVALID,
                            format!("invalid hire request in event {}: {error}", event.id),
                        )
                    })?;
                    if request.project_id != project_id || request.id.trim().is_empty() {
                        return Err(employment_error(
                            agent_identity_codes::IDENTITY_INVALID,
                            format!(
                                "hire request in event {} has a mismatched project or empty id",
                                event.id
                            ),
                        ));
                    }
                    request.origin = "local".to_string();
                    requests.insert(request.id.clone(), request);
                }
            }
            AGENT_RETIREMENT_PROPOSED_EVENT
            | AGENT_RETIREMENT_APPROVED_EVENT
            | AGENT_RETIREMENT_REJECTED_EVENT => {
                if let Some(proposal) = event.payload.get("proposal") {
                    let mut proposal =
                        serde_json::from_value::<AgentRetirementProposal>(proposal.clone())
                            .map_err(|error| {
                                employment_error(
                                    agent_identity_codes::IDENTITY_INVALID,
                                    format!(
                                        "invalid retirement proposal in event {}: {error}",
                                        event.id
                                    ),
                                )
                            })?;
                    if proposal.project_id != project_id || proposal.id.trim().is_empty() {
                        return Err(employment_error(
                            agent_identity_codes::IDENTITY_INVALID,
                            format!(
                                "retirement proposal in event {} has a mismatched project or empty id",
                                event.id
                            ),
                        ));
                    }
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
    Ok((
        requests.into_values().collect(),
        proposals.into_values().collect(),
    ))
}

/// Newest employment events for one identity, returned oldest-first. The
/// identity detail view uses this so a project-wide event cap cannot hide a
/// specific identity's recent history.
pub fn list_employment_events_for_identity(
    store: &Store,
    project_id: &str,
    identity_id: &str,
    limit: u32,
) -> Result<Vec<AgentEmploymentEvent>> {
    let limit = limit.clamp(1, 100_000) as i64;
    let conn = store.lock()?;
    let mut statement = conn.prepare(
        "SELECT id, project_id, seq, identity_id, kind, payload, created_at
         FROM agent_employment_events
         WHERE project_id = ?1 AND identity_id = ?2
         ORDER BY seq DESC LIMIT ?3",
    )?;
    let mut events = statement
        .query_map(params![project_id.trim(), identity_id, limit], |row| {
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
        })?
        .collect::<Result<Vec<_>, _>>()?;
    events.reverse();
    Ok(events)
}

pub fn count_employment_events(store: &Store, project_id: &str) -> Result<u64> {
    let conn = store.lock()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_employment_events WHERE project_id = ?1",
        params![project_id.trim()],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u64)
}

/// Rebuild hire-request and proposal projections from the append-only
/// employment stream. Hire requests are reconstructed from the latest snapshot
/// carried by a requested/approved/rejected event; proposals from their
/// proposed/approved/rejected/executed events. Imported evidence rows are
/// preserved and never overwritten by a local rebuild.
pub fn rebuild_employment_projections(store: &Store, project_id: &str) -> Result<()> {
    let events = list_employment_events(store, project_id, None)?;
    let total = count_employment_events(store, project_id)?;
    if total > events.len() as u64 {
        return Err(employment_error(
            agent_identity_codes::EMPLOYMENT_REPLAY_TRUNCATED,
            format!(
                "project has {total} employment events but replay reads at most {}; refusing to rebuild from a truncated stream",
                events.len()
            ),
        ));
    }
    let (requests, proposals) = employment_projections_from_events(project_id, &events)?;
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
        employment_projections_from_events(project_id, &events)?;
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

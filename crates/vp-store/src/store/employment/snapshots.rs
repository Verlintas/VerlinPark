use super::*;

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

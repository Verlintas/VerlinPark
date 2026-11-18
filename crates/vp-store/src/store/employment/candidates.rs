use super::*;

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

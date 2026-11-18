use super::*;

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
            super::super::AGENT_IDENTITY_COLUMNS
        ))?;
        statement
            .query_map(params![project_id], super::super::map_agent_identity)?
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
            "{ASSIGNMENT_SELECT}
             WHERE s.project_id = ?1
               AND (a.status = 'active' OR a.assigned_at >= ?2)
             ORDER BY a.session_id ASC, a.identity_id ASC LIMIT ?3"
        ))?;
        statement
            .query_map(
                params![project_id, cutoff, MAX_PERFORMANCE_EVIDENCE_ROWS as i64],
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
                    // C6 emits `unsubstantiated_handoff` (singular); the S6
                    // retirement policy constant historically used the plural
                    // form. Accept both so the threshold is not dead code.
                    if reason == agent_retirement_reasons::UNSUBSTANTIATED_HANDOFFS
                        || reason == vp_protocol::diagnosis::reason_codes::UNSUBSTANTIATED_HANDOFF
                    {
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

pub(super) fn insert_agent_performance_row_tx(
    tx: &Transaction<'_>,
    row: &AgentPerformance,
) -> Result<()> {
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

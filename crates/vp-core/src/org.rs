//! S7 project org chart and identity console read model.
//!
//! This module is a bounded, read-only projection over the S1–S6 stores. It
//! answers "who owns what, who is idle or retired, and what has this identity
//! done" without adding a second org store, changing any gate, or calling a
//! provider. Every mutation remains an explicit S6 RPC.
//!
//! Security invariants:
//!
//! - only identity metadata, assignment/lease metadata, workspace branch and
//!   status, inbox metadata, memory rows, performance counts, and employment
//!   evidence are read;
//! - prompts, tool arguments, tool output, file content, absolute worktree
//!   paths, lease tokens, and credentials are never copied into a result;
//! - inbox content is opt-in and reduced to a short known-field preview;
//! - every collection is capped, and truncation is reported rather than hidden.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use serde_json::Value;
use time::OffsetDateTime;
use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AGENT_ORG_DEFAULT_DETAIL_ASSIGNMENTS, AGENT_ORG_DEFAULT_DETAIL_EMPLOYMENT_EVENTS,
    AGENT_ORG_DEFAULT_DETAIL_HIRE_REQUESTS, AGENT_ORG_DEFAULT_DETAIL_INBOX,
    AGENT_ORG_DEFAULT_DETAIL_MEMORY, AGENT_ORG_DEFAULT_DETAIL_PROPOSALS, AGENT_ORG_MAX_DEPARTMENTS,
    AGENT_ORG_MAX_DETAIL_ASSIGNMENTS, AGENT_ORG_MAX_DETAIL_INBOX, AGENT_ORG_MAX_DETAIL_MEMORY,
    AGENT_ORG_MAX_DETAIL_PROPOSALS, AGENT_ORG_MAX_HIRE_REQUESTS, AGENT_ORG_MAX_NODES,
    AGENT_ORG_MAX_WORKSPACES, AgentAssignment, AgentHireRequest, AgentIdentity,
    AgentIdentityDetail, AgentIdentityDetailParams, AgentIdentityError, AgentIdentityInboxEntry,
    AgentIdentityPolicy, AgentOrgAssignmentState, AgentOrgCounts, AgentOrgDepartment, AgentOrgNode,
    AgentOrgPerformanceSummary, AgentOrgRetirementState, AgentOrgSnapshot, AgentOrgSnapshotParams,
    AgentOrgWorkspaceState, AgentPerformance, AgentRetirementCandidate, AgentRetirementPolicy,
    AgentRetirementProposal, AgentRetirementProposalListParams, AgentWorkspace,
    agent_identity_codes,
};
use vp_store::Store;

use crate::Engine;

const MAX_INBOX_SUMMARY_CHARS: usize = 200;
const UNASSIGNED_DEPARTMENT: &str = "(unassigned)";

fn org_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentIdentityError::new(code, message).into()
}

/// Loaded evidence shared by the org snapshot and identity detail. It is one
/// bounded read of existing projections; it is not a cache and it never mutates.
struct OrgContext {
    project_id: String,
    policy: AgentIdentityPolicy,
    retirement_policy: AgentRetirementPolicy,
    identities: Vec<AgentIdentity>,
    performance_by_id: HashMap<String, AgentPerformance>,
    candidates: Vec<AgentRetirementCandidate>,
    proposals: Vec<AgentRetirementProposal>,
    assignments: Vec<AgentAssignment>,
    assignment_by_identity: HashMap<String, AgentAssignment>,
    assignment_counts: HashMap<String, u64>,
    session_ids_by_identity: HashMap<String, BTreeSet<String>>,
    workspaces: Vec<AgentWorkspace>,
    workspaces_by_identity: HashMap<String, Vec<AgentWorkspace>>,
    unread_inbox: HashMap<String, u64>,
    memory_counts: HashMap<String, u64>,
    pending_hire_requests: Vec<AgentHireRequest>,
}

impl OrgContext {
    fn performance(&self, identity_id: &str) -> AgentPerformance {
        self.performance_by_id
            .get(identity_id)
            .cloned()
            .unwrap_or_default()
    }
}

fn load_context(store: &Store, project_id: &str) -> Result<OrgContext> {
    let project_id = project_id.trim();
    if project_id.is_empty() {
        return Err(org_error(
            agent_identity_codes::ORG_INVALID,
            "project_id is required for an org snapshot",
        ));
    }
    if store.get_project(project_id)?.is_none() {
        return Err(org_error(
            agent_identity_codes::ORG_PROJECT_NOT_FOUND,
            format!("project not found: {project_id}"),
        ));
    }
    let policy = store.get_agent_identity_policy(project_id)?;
    let retirement_policy =
        vp_store::store::employment::get_retirement_policy(store, project_id)?.policy;
    let identities = store.list_agent_identities(project_id, None, None)?;
    let now = OffsetDateTime::now_utc();
    let performances = vp_store::store::employment::compute_agent_performance(
        store,
        project_id,
        retirement_policy.lookback_days,
        now,
    )?;
    let performance_by_id = performances
        .iter()
        .map(|row| (row.identity_id.clone(), row.clone()))
        .collect::<HashMap<_, _>>();

    // Candidates are deterministic and advisory. They are computed here only
    // because the org chart shows the advisory state; no proposal is created.
    let last_proposal_at =
        vp_store::store::employment::last_retirement_proposal_times(store, project_id)?;
    let local_identities = identities
        .iter()
        .filter(|identity| identity.origin == "local")
        .cloned()
        .collect::<Vec<_>>();
    let candidates = vp_store::store::employment::retirement_candidates(
        project_id,
        &local_identities,
        &performances,
        &retirement_policy,
        &last_proposal_at,
        now,
        Some(AGENT_ORG_MAX_NODES),
    );

    let proposals = vp_store::store::employment::list_retirement_proposals(
        store,
        &AgentRetirementProposalListParams {
            project_id: project_id.to_string(),
            limit: Some(AGENT_ORG_MAX_DETAIL_PROPOSALS.max(AGENT_ORG_MAX_NODES)),
            ..AgentRetirementProposalListParams::default()
        },
    )?
    .proposals;

    let assignments = store.list_session_agent_assignments(
        None,
        None,
        Some(project_id),
        Some("active"),
        Some(AGENT_ORG_MAX_NODES),
    )?;
    let mut assignment_by_identity: HashMap<String, AgentAssignment> = HashMap::new();
    let mut assignment_counts = HashMap::new();
    let mut session_ids_by_identity: HashMap<String, BTreeSet<String>> = HashMap::new();
    for assignment in &assignments {
        if assignment.status != "active" {
            continue;
        }
        *assignment_counts
            .entry(assignment.identity_id.clone())
            .or_insert(0) += 1;
        session_ids_by_identity
            .entry(assignment.identity_id.clone())
            .or_default()
            .insert(assignment.session_id.clone());
        let replace = match assignment_by_identity.get(&assignment.identity_id) {
            None => true,
            Some(current) => {
                (assignment.lease_active, &assignment.assigned_at)
                    > (current.lease_active, &current.assigned_at)
            }
        };
        if replace {
            assignment_by_identity.insert(assignment.identity_id.clone(), assignment.clone());
        }
    }

    let workspaces = store.list_agent_workspaces(
        Some(project_id),
        None,
        None,
        Some(AGENT_ORG_MAX_WORKSPACES),
    )?;
    let mut workspaces_by_identity: HashMap<String, Vec<AgentWorkspace>> = HashMap::new();
    for workspace in &workspaces {
        workspaces_by_identity
            .entry(workspace.identity_id.clone())
            .or_default()
            .push(workspace.clone());
    }
    for rows in workspaces_by_identity.values_mut() {
        rows.sort_by(|left, right| {
            (right.status == "active")
                .cmp(&(left.status == "active"))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.id.cmp(&right.id))
        });
    }

    let unread_inbox = store.unread_agent_inbox_counts_for_project(project_id)?;
    let memory_counts = vp_store::store::identity_memory::active_agent_memory_counts_by_identity(
        store, project_id,
    )?;

    let pending_hire_requests = vp_store::store::employment::list_agent_hire_requests(
        store,
        &vp_protocol::types::AgentHireListParams {
            identity_id: None,
            project_id: project_id.to_string(),
            status: Some("pending".to_string()),
            limit: Some(AGENT_ORG_MAX_HIRE_REQUESTS),
        },
    )?
    .requests;

    Ok(OrgContext {
        project_id: project_id.to_string(),
        policy,
        retirement_policy,
        identities,
        performance_by_id,
        candidates,
        proposals,
        assignments,
        assignment_by_identity,
        assignment_counts,
        session_ids_by_identity,
        workspaces,
        workspaces_by_identity,
        unread_inbox,
        memory_counts,
        pending_hire_requests,
    })
}

fn workspace_state(workspace: &AgentWorkspace, count: u64) -> AgentOrgWorkspaceState {
    AgentOrgWorkspaceState {
        id: workspace.id.clone(),
        identity_id: workspace.identity_id.clone(),
        component: workspace.component.clone(),
        branch: workspace.branch.clone(),
        status: workspace.status.clone(),
        lease_active: workspace.lease_active,
        lease_expires_at: workspace.lease_expires_at.clone(),
        last_session_id: workspace.last_session_id.clone(),
        workspace_count: count,
    }
}

fn retirement_state(
    context: &OrgContext,
    identity_id: &str,
    candidate: Option<&AgentRetirementCandidate>,
    proposals: &[AgentRetirementProposal],
) -> AgentOrgRetirementState {
    let mut state = AgentOrgRetirementState::default();
    if let Some(candidate) = candidate {
        state.candidate = true;
        state.severity = candidate.severity;
        state.reasons = candidate.reasons.clone();
    }
    let mut matching = proposals
        .iter()
        .filter(|proposal| proposal.identity_id == identity_id)
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        right
            .proposed_at
            .cmp(&left.proposed_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    state.pending_proposals = matching
        .iter()
        .filter(|proposal| proposal.status == "pending")
        .count() as u64;
    if let Some(latest) = matching.first() {
        state.latest_proposal_id = Some(latest.id.clone());
        state.latest_proposal_status = Some(latest.status.clone());
        state.latest_proposal_at = Some(latest.proposed_at.clone());
    }
    let _ = context;
    state
}

fn node_for_identity(context: &OrgContext, identity: &AgentIdentity) -> AgentOrgNode {
    let performance = context.performance(&identity.id);
    let assignment = context.assignment_by_identity.get(&identity.id);
    let active_assignment = assignment.map(|assignment| AgentOrgAssignmentState {
        session_id: assignment.session_id.clone(),
        role: assignment.role.clone(),
        assigned_at: assignment.assigned_at.clone(),
        lease_expires_at: assignment.lease_expires_at.clone(),
        lease_active: assignment.lease_active,
        assignment_count: *context.assignment_counts.get(&identity.id).unwrap_or(&0),
        session_count: context
            .session_ids_by_identity
            .get(&identity.id)
            .map(|sessions| sessions.len() as u64)
            .unwrap_or(0),
    });
    let workspace_rows = context
        .workspaces_by_identity
        .get(&identity.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let workspace = workspace_rows
        .first()
        .map(|workspace| workspace_state(workspace, workspace_rows.len() as u64));
    let candidate = context
        .candidates
        .iter()
        .find(|candidate| candidate.identity_id == identity.id);
    AgentOrgNode {
        identity: identity.clone(),
        active_assignment,
        workspace,
        unread_inbox: *context.unread_inbox.get(&identity.id).unwrap_or(&0),
        memory_count: *context.memory_counts.get(&identity.id).unwrap_or(&0),
        performance: AgentOrgPerformanceSummary::from(&performance),
        retirement: retirement_state(context, &identity.id, candidate, &context.proposals),
        reports: Vec::new(),
        report_count: 0,
    }
}

fn sort_nodes(nodes: &mut [AgentOrgNode]) {
    nodes.sort_by(|left, right| {
        role_rank(&left.identity.role)
            .cmp(&role_rank(&right.identity.role))
            .then_with(|| {
                left.identity
                    .department
                    .as_deref()
                    .unwrap_or("")
                    .cmp(right.identity.department.as_deref().unwrap_or(""))
            })
            .then_with(|| {
                left.identity
                    .component
                    .as_deref()
                    .unwrap_or("")
                    .cmp(right.identity.component.as_deref().unwrap_or(""))
            })
            .then_with(|| left.identity.id.cmp(&right.identity.id))
    });
}

fn role_rank(role: &str) -> u8 {
    match role {
        "leader" => 0,
        "manager" => 1,
        "worker" => 2,
        _ => 3,
    }
}

fn build_departments(
    context: &OrgContext,
    included: &HashMap<String, AgentIdentity>,
    warnings: &mut Vec<String>,
) -> Vec<AgentOrgDepartment> {
    let mut departments: BTreeMap<String, AgentOrgDepartment> = BTreeMap::new();
    for identity in context.identities.iter() {
        if !included.contains_key(&identity.id) {
            continue;
        }
        let name = identity
            .department
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(UNASSIGNED_DEPARTMENT)
            .to_string();
        let entry = departments
            .entry(name.clone())
            .or_insert_with(|| AgentOrgDepartment {
                name,
                ..AgentOrgDepartment::default()
            });
        if identity.role == "manager" {
            entry.manager_count += 1;
            entry.manager_ids.push(identity.id.clone());
        } else if identity.role == "worker" {
            entry.worker_count += 1;
            entry.worker_ids.push(identity.id.clone());
            if identity.status == "active" {
                entry.active_workers += 1;
            }
        }
        if identity.status != "retired" {
            entry.active_count += 1;
        }
        entry.unread_inbox += *context.unread_inbox.get(&identity.id).unwrap_or(&0);
    }
    let mut rows = departments.into_values().collect::<Vec<_>>();
    if rows.len() > AGENT_ORG_MAX_DEPARTMENTS as usize {
        rows.truncate(AGENT_ORG_MAX_DEPARTMENTS as usize);
        warnings.push(format!(
            "department lanes were capped at {AGENT_ORG_MAX_DEPARTMENTS}"
        ));
    }
    for row in &mut rows {
        row.manager_ids.sort();
        row.manager_ids.dedup();
        row.worker_ids.sort();
        row.worker_ids.dedup();
    }
    rows
}

fn build_snapshot(
    context: &OrgContext,
    include_retired: bool,
    limit: Option<u32>,
) -> AgentOrgSnapshot {
    let limit = limit
        .unwrap_or(AGENT_ORG_MAX_NODES)
        .clamp(1, AGENT_ORG_MAX_NODES) as usize;
    let mut warnings = Vec::new();
    let mut included: HashMap<String, AgentIdentity> = HashMap::new();
    let mut eligible = context
        .identities
        .iter()
        .filter(|identity| include_retired || identity.status != "retired")
        .cloned()
        .collect::<Vec<_>>();
    // Truncation must never hide the chart root or department managers behind
    // a wall of older Workers. Role order is stable, then creation order.
    eligible.sort_by(|left, right| {
        role_rank(&left.role)
            .cmp(&role_rank(&right.role))
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut truncated = eligible.len() > limit;
    let filtered = eligible.into_iter().take(limit).collect::<Vec<_>>();
    for identity in &filtered {
        included.insert(identity.id.clone(), identity.clone());
    }

    let mut nodes: HashMap<String, AgentOrgNode> = filtered
        .iter()
        .map(|identity| (identity.id.clone(), node_for_identity(context, identity)))
        .collect();

    let mut leader_id: Option<String> = None;
    for identity in &filtered {
        if identity.role == "leader" && identity.status != "retired" {
            if leader_id.is_none() {
                leader_id = Some(identity.id.clone());
            } else {
                warnings.push(
                    "more than one live Leader row exists; only the first is the chart root"
                        .to_string(),
                );
            }
        }
    }

    // Attach workers to their Manager parent. Workers whose parent is missing,
    // retired, hidden, or not a Manager stay visible as unassigned roots.
    let manager_ids = filtered
        .iter()
        .filter(|identity| identity.role == "manager")
        .map(|identity| identity.id.clone())
        .collect::<BTreeSet<_>>();
    let mut workers_by_manager: HashMap<String, Vec<String>> = HashMap::new();
    let mut unassigned_ids: Vec<String> = Vec::new();
    for identity in &filtered {
        if identity.role != "worker" {
            continue;
        }
        let parent = identity.parent_identity_id.as_deref();
        match parent {
            Some(parent) if manager_ids.contains(parent) && included.contains_key(parent) => {
                workers_by_manager
                    .entry(parent.to_string())
                    .or_default()
                    .push(identity.id.clone());
            }
            _ => unassigned_ids.push(identity.id.clone()),
        }
    }

    let mut unassigned_managers: Vec<String> = Vec::new();
    for identity in &filtered {
        if identity.role != "manager" {
            continue;
        }
        let attached = leader_id.as_deref().is_some_and(|leader| {
            identity.parent_identity_id.as_deref() == Some(leader)
                && identity.id != leader
                && identity.status != "retired"
        });
        if !attached {
            unassigned_managers.push(identity.id.clone());
        }
    }

    let mut manager_nodes: Vec<AgentOrgNode> = Vec::new();
    for manager_id in &manager_ids {
        if unassigned_managers.contains(manager_id) {
            continue;
        }
        let Some(mut node) = nodes.remove(manager_id) else {
            continue;
        };
        let mut reports = workers_by_manager
            .remove(manager_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|worker_id| nodes.remove(&worker_id))
            .collect::<Vec<_>>();
        sort_nodes(&mut reports);
        node.report_count = reports.len() as u64;
        node.reports = reports;
        manager_nodes.push(node);
    }
    sort_nodes(&mut manager_nodes);

    let leader = leader_id.as_ref().and_then(|leader_id| {
        let mut node = nodes.remove(leader_id)?;
        node.report_count = manager_nodes.len() as u64;
        node.reports = manager_nodes.clone();
        Some(node)
    });

    let mut unassigned = Vec::new();
    for manager_id in unassigned_managers {
        if let Some(mut node) = nodes.remove(&manager_id) {
            let mut reports = workers_by_manager
                .remove(&manager_id)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|worker_id| nodes.remove(&worker_id))
                .collect::<Vec<_>>();
            sort_nodes(&mut reports);
            node.report_count = reports.len() as u64;
            node.reports = reports;
            unassigned.push(node);
        }
    }
    for worker_id in unassigned_ids {
        if let Some(node) = nodes.remove(&worker_id) {
            unassigned.push(node);
        }
    }
    for identity in &filtered {
        if let Some(node) = nodes.remove(&identity.id)
            && Some(&identity.id) != leader_id.as_ref()
        {
            unassigned.push(node);
        }
    }
    sort_nodes(&mut unassigned);

    let departments = build_departments(context, &included, &mut warnings);

    let active_assignments = context
        .assignments
        .iter()
        .filter(|assignment| {
            included.contains_key(&assignment.identity_id)
                && assignment.status == "active"
                && assignment.lease_active
        })
        .take(AGENT_ORG_MAX_NODES as usize)
        .cloned()
        .collect::<Vec<_>>();
    if active_assignments.len() >= AGENT_ORG_MAX_NODES as usize {
        truncated = true;
    }

    let mut workspace_summaries = context
        .workspaces
        .iter()
        .filter(|workspace| included.contains_key(&workspace.identity_id))
        .map(|workspace| {
            let count = context
                .workspaces_by_identity
                .get(&workspace.identity_id)
                .map(Vec::len)
                .unwrap_or(1) as u64;
            workspace_state(workspace, count)
        })
        .collect::<Vec<_>>();
    if workspace_summaries.len() > AGENT_ORG_MAX_WORKSPACES as usize {
        workspace_summaries.truncate(AGENT_ORG_MAX_WORKSPACES as usize);
        truncated = true;
    }

    let mut counts = AgentOrgCounts::default();
    for identity in &filtered {
        counts.identities += 1;
        match identity.role.as_str() {
            "leader" => counts.leaders += 1,
            "manager" => counts.managers += 1,
            "worker" => counts.workers += 1,
            _ => {}
        }
        match identity.status.as_str() {
            "active" => counts.active += 1,
            "idle" => counts.idle += 1,
            "paused" => counts.paused += 1,
            "retired" => counts.retired += 1,
            _ => {}
        }
        if identity.origin == "imported" {
            counts.imported += 1;
        }
        counts.unread_inbox += *context.unread_inbox.get(&identity.id).unwrap_or(&0);
        counts.memory_count += *context.memory_counts.get(&identity.id).unwrap_or(&0);
    }
    counts.active_assignments = active_assignments.len() as u64;
    counts.active_workspaces = context
        .workspaces
        .iter()
        .filter(|workspace| {
            included.contains_key(&workspace.identity_id) && workspace.status == "active"
        })
        .count() as u64;
    counts.pending_hire_requests = context.pending_hire_requests.len() as u64;
    counts.pending_retirement_proposals = context
        .proposals
        .iter()
        .filter(|proposal| proposal.status == "pending")
        .count() as u64;
    counts.retirement_candidates = context
        .candidates
        .iter()
        .filter(|candidate| included.contains_key(&candidate.identity_id))
        .count() as u64;

    if !context.policy.enabled {
        warnings.push(
            "project identity policy is disabled; the chart is a read-only projection of existing rows"
                .to_string(),
        );
    }
    if leader.is_none() {
        warnings.push("no live Leader identity is recorded for this project".to_string());
    }
    if counts.imported > 0 {
        warnings.push(format!(
            "{} imported identities are read-only archive evidence",
            counts.imported
        ));
    }
    if !include_retired
        && counts.retired == 0
        && context.identities.iter().any(|row| row.status == "retired")
    {
        warnings.push(
            "retired identities are hidden; opt in to inspect retirement evidence".to_string(),
        );
    }
    if !unassigned.is_empty() {
        warnings.push(format!(
            "{} identities have no parent in the active hierarchy",
            unassigned.len()
        ));
    }
    if truncated {
        warnings.push(format!(
            "the org snapshot is bounded to {limit} identities and {} assignments/workspaces",
            AGENT_ORG_MAX_NODES
        ));
    }

    let pending_retirement_proposals = context
        .proposals
        .iter()
        .filter(|proposal| proposal.status == "pending")
        .cloned()
        .collect::<Vec<_>>();

    AgentOrgSnapshot {
        project_id: context.project_id.clone(),
        generated_at: now_rfc3339(),
        enabled: context.policy.enabled,
        policy: context.policy.clone(),
        retirement_policy: context.retirement_policy.clone(),
        counts,
        leader,
        unassigned,
        departments,
        active_assignments,
        workspace_summaries,
        pending_hire_requests: context.pending_hire_requests.clone(),
        pending_retirement_proposals,
        truncated,
        warnings,
    }
}

fn find_node<'a>(node: &'a AgentOrgNode, identity_id: &str) -> Option<&'a AgentOrgNode> {
    if node.identity.id == identity_id {
        return Some(node);
    }
    for report in &node.reports {
        if let Some(found) = find_node(report, identity_id) {
            return Some(found);
        }
    }
    None
}

fn inbox_summary(payload: &Value) -> Option<String> {
    for key in [
        "summary", "title", "body", "message", "question", "decision",
    ] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.chars().take(MAX_INBOX_SUMMARY_CHARS).collect());
            }
        }
    }
    None
}

fn detail_error(message: impl Into<String>) -> anyhow::Error {
    org_error(agent_identity_codes::DETAIL_INVALID, message)
}

impl Engine {
    /// Bounded project org snapshot. This is a read-only projection; it does
    /// not assign, hire, retire, propose, repair, or call a model.
    pub async fn agent_org_snapshot(
        &self,
        params: AgentOrgSnapshotParams,
    ) -> Result<AgentOrgSnapshot> {
        let store = self.store.clone();
        let project_id = params.project_id.trim().to_string();
        let include_retired = params.include_retired;
        let limit = params.limit;
        tokio::task::spawn_blocking(move || {
            let context = load_context(&store, &project_id)?;
            Ok(build_snapshot(&context, include_retired, limit))
        })
        .await?
    }

    /// Bounded identity detail for the operator console. Inbox payload content
    /// is only reduced to a short known-field summary when explicitly requested.
    pub async fn agent_identity_detail(
        &self,
        params: AgentIdentityDetailParams,
    ) -> Result<AgentIdentityDetail> {
        let store = self.store.clone();
        let identity_id = params.identity_id.trim().to_string();
        if identity_id.is_empty() {
            return Err(detail_error("identity_id is required"));
        }
        let requested_project = params.project_id.clone();
        let include_inbox_content = params.include_inbox_content;
        let inbox_limit = params
            .inbox_limit
            .unwrap_or(AGENT_ORG_DEFAULT_DETAIL_INBOX)
            .clamp(1, AGENT_ORG_MAX_DETAIL_INBOX);
        let memory_limit = params
            .memory_limit
            .unwrap_or(AGENT_ORG_DEFAULT_DETAIL_MEMORY)
            .clamp(1, AGENT_ORG_MAX_DETAIL_MEMORY);
        let assignment_limit = params
            .assignment_limit
            .unwrap_or(AGENT_ORG_DEFAULT_DETAIL_ASSIGNMENTS)
            .clamp(1, AGENT_ORG_MAX_DETAIL_ASSIGNMENTS);
        tokio::task::spawn_blocking(move || {
            let identity = store.get_agent_identity(&identity_id)?.ok_or_else(|| {
                org_error(
                    agent_identity_codes::IDENTITY_NOT_FOUND,
                    format!("agent identity not found: {identity_id}"),
                )
            })?;
            if let Some(project_id) = requested_project.as_deref()
                && project_id != identity.project_id
            {
                return Err(org_error(
                    agent_identity_codes::ORG_PROJECT_MISMATCH,
                    "identity belongs to another project",
                ));
            }
            let project_id = identity.project_id.clone();
            let context = load_context(&store, &project_id)?;
            let mut warnings = Vec::new();
            let mut truncated = false;

            let snapshot = build_snapshot(&context, true, Some(AGENT_ORG_MAX_NODES));
            let mut node = snapshot
                .leader
                .as_ref()
                .and_then(|leader| find_node(leader, &identity.id))
                .cloned()
                .or_else(|| {
                    snapshot
                        .unassigned
                        .iter()
                        .find_map(|row| find_node(row, &identity.id))
                        .cloned()
                })
                .unwrap_or_else(|| node_for_identity(&context, &identity));

            let assignments = store.list_session_agent_assignments(
                None,
                Some(&identity.id),
                None,
                None,
                Some(assignment_limit),
            )?;

            let (inbox_messages, inbox_counts) = store.list_agent_inbox_messages(
                Some(&project_id),
                Some(&identity.id),
                None,
                None,
                false,
                Some(inbox_limit),
            )?;
            let inbox = inbox_messages
                .iter()
                .map(|message| AgentIdentityInboxEntry {
                    id: message.id.clone(),
                    kind: message.kind.clone(),
                    status: message.status.clone(),
                    session_id: message.session_id.clone(),
                    from_identity_id: message.from_identity_id.clone(),
                    from_label: message.from_label.clone(),
                    created_at: message.created_at.clone(),
                    read_at: message.read_at.clone(),
                    expires_at: message.expires_at.clone(),
                    summary: if include_inbox_content {
                        inbox_summary(&message.payload)
                    } else {
                        None
                    },
                })
                .collect::<Vec<_>>();
            if inbox_messages.len() as u32 >= inbox_limit {
                truncated = true;
            }

            let memories = vp_store::store::identity_memory::list_visible_agent_memories(
                &store,
                &project_id,
                &identity.id,
                identity.department.as_deref(),
                Some("active"),
                false,
                Some(memory_limit),
            )?;
            if memories.len() as u32 >= memory_limit {
                truncated = true;
            }
            let memory_total = vp_store::store::identity_memory::visible_agent_memory_count(
                &store,
                &project_id,
                &identity.id,
                identity.department.as_deref(),
            )?;

            let performance = context.performance(&identity.id);
            let retirement_candidates = context
                .candidates
                .iter()
                .filter(|candidate| candidate.identity_id == identity.id)
                .cloned()
                .collect::<Vec<_>>();
            let retirement_proposals = vp_store::store::employment::list_retirement_proposals(
                &store,
                &AgentRetirementProposalListParams {
                    project_id: project_id.clone(),
                    identity_id: Some(identity.id.clone()),
                    limit: Some(AGENT_ORG_DEFAULT_DETAIL_PROPOSALS),
                    ..AgentRetirementProposalListParams::default()
                },
            )?
            .proposals;
            if retirement_proposals.len() as u32 >= AGENT_ORG_DEFAULT_DETAIL_PROPOSALS {
                truncated = true;
            }

            let hire_requests = vp_store::store::employment::list_agent_hire_requests(
                &store,
                &vp_protocol::types::AgentHireListParams {
                    project_id: project_id.clone(),
                    identity_id: Some(identity.id.clone()),
                    limit: Some(AGENT_ORG_DEFAULT_DETAIL_HIRE_REQUESTS),
                    ..vp_protocol::types::AgentHireListParams::default()
                },
            )?
            .requests;

            let workspaces = context
                .workspaces_by_identity
                .get(&identity.id)
                .map(|rows| {
                    rows.iter()
                        .map(|workspace| workspace_state(workspace, rows.len() as u64))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            let parent = identity
                .parent_identity_id
                .as_deref()
                .and_then(|parent_id| store.get_agent_identity(parent_id).ok().flatten())
                .filter(|parent| parent.project_id == identity.project_id);
            let successor = identity
                .successor_id
                .as_deref()
                .and_then(|successor_id| store.get_agent_identity(successor_id).ok().flatten())
                .filter(|successor| successor.project_id == identity.project_id);
            let mut reports = context
                .identities
                .iter()
                .filter(|row| row.parent_identity_id.as_deref() == Some(identity.id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            reports.sort_by(|left, right| {
                role_rank(&left.role)
                    .cmp(&role_rank(&right.role))
                    .then_with(|| left.id.cmp(&right.id))
            });
            reports.truncate(AGENT_ORG_MAX_NODES as usize);

            let employment_events =
                vp_store::store::employment::list_employment_events_for_identity(
                    &store,
                    &project_id,
                    &identity.id,
                    AGENT_ORG_DEFAULT_DETAIL_EMPLOYMENT_EVENTS,
                )?;

            let (read_only, read_only_reason) = if identity.origin == "imported" {
                (
                    true,
                    Some(
                        "imported identities are read-only archive evidence until an explicit rehome"
                            .to_string(),
                    ),
                )
            } else if identity.status == "retired" {
                (
                    true,
                    Some("retired identities are terminal; evidence is preserved".to_string()),
                )
            } else {
                (false, None)
            };
            if !context.policy.enabled {
                warnings.push(
                    "project identity policy is disabled; all mutations still require explicit policy/RPC approval"
                        .to_string(),
                );
            }
            if identity.origin == "imported" {
                warnings.push("this identity is imported and cannot be changed locally".to_string());
            }
            if truncated {
                warnings.push(
                    "one or more detail lists reached their bound; narrow the request or use the CLI"
                        .to_string(),
                );
            }

            // The nested node carries the whole sub-tree; the detail payload
            // also carries the direct reports separately for the operator.
            node.reports = reports
                .iter()
                .map(|report| node_for_identity(&context, report))
                .collect();
            node.report_count = reports.len() as u64;

            Ok(AgentIdentityDetail {
                project_id,
                generated_at: now_rfc3339(),
                identity: identity.clone(),
                policy: context.policy.clone(),
                retirement_policy: context.retirement_policy.clone(),
                node,
                assignments,
                inbox,
                inbox_counts,
                memories,
                memory_total,
                performance,
                retirement_candidates,
                retirement_proposals,
                hire_requests,
                workspaces,
                parent,
                successor,
                reports,
                employment_events,
                read_only,
                read_only_reason,
                truncated,
                warnings,
            })
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AgentHireParams, AgentIdentityPolicy, AgentRetirementPolicy, AgentTransferParams,
        CreateAgentIdentityParams, CreateAgentMemoryParams, MemoryEvidence, SessionAssignParams,
    };

    fn init_repo(path: &std::path::Path) {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "org@test.local"],
            vec!["config", "user.name", "Org Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(path)
                .status()
                .expect("git");
            assert!(status.success(), "git {:?} failed", args);
        }
    }

    fn test_engine(dir: &TempDir) -> (Engine, String) {
        let store = Store::open(dir.path()).expect("store");
        let engine = Engine::new(store);
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("repo");
        std::fs::write(root.join("README.md"), "org test\n").expect("readme");
        init_repo(&root);
        let project = engine
            .store()
            .add_project(root.to_str().expect("path"), Some("org"))
            .expect("project");
        (engine, project.id)
    }

    async fn enable_identity_policy(engine: &Engine, project_id: &str) {
        engine
            .set_agent_identity_policy(
                project_id,
                AgentIdentityPolicy {
                    enabled: true,
                    ..AgentIdentityPolicy::default()
                },
            )
            .await
            .expect("policy");
    }

    async fn create_identity(
        engine: &Engine,
        project_id: &str,
        role: &str,
        title: &str,
        department: Option<&str>,
        component: Option<&str>,
        parent: Option<&str>,
    ) -> String {
        engine
            .create_agent_identity(CreateAgentIdentityParams {
                project_id: project_id.to_string(),
                role: role.to_string(),
                title: Some(title.to_string()),
                department: department.map(str::to_string),
                component: component.map(str::to_string),
                profile: Some("coding".to_string()),
                model: Some("test-model".to_string()),
                parent_identity_id: parent.map(str::to_string),
                ..CreateAgentIdentityParams::default()
            })
            .await
            .expect("identity")
            .identity
            .id
    }

    async fn hierarchy(engine: &Engine, project_id: &str) -> (String, String, String) {
        enable_identity_policy(engine, project_id).await;
        let leader =
            create_identity(engine, project_id, "leader", "Leader", None, None, None).await;
        let manager = create_identity(
            engine,
            project_id,
            "manager",
            "Platform Manager",
            Some("Platform"),
            Some("platform"),
            Some(&leader),
        )
        .await;
        let worker = create_identity(
            engine,
            project_id,
            "worker",
            "Auth Worker",
            Some("Platform"),
            Some("auth"),
            Some(&manager),
        )
        .await;
        (leader, manager, worker)
    }

    #[tokio::test]
    async fn org_snapshot_builds_leader_manager_worker_hierarchy() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (leader, manager, worker) = hierarchy(&engine, &project_id).await;

        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: project_id.clone(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect("snapshot");

        let root = snapshot.leader.as_ref().expect("leader root");
        assert_eq!(root.identity.id, leader);
        assert_eq!(root.report_count, 1);
        assert_eq!(root.reports[0].identity.id, manager);
        assert_eq!(root.reports[0].reports[0].identity.id, worker);
        assert_eq!(snapshot.counts.leaders, 1);
        assert_eq!(snapshot.counts.managers, 1);
        assert_eq!(snapshot.counts.workers, 1);
        assert!(snapshot.unassigned.is_empty());
        assert_eq!(snapshot.departments.len(), 2);
        assert_eq!(snapshot.departments[0].name, UNASSIGNED_DEPARTMENT);
        assert_eq!(snapshot.departments[1].name, "Platform");
        assert_eq!(snapshot.departments[1].worker_ids, vec![worker.clone()]);
        assert!(snapshot.enabled);
        assert!(snapshot.project_id == project_id);
        assert!(!snapshot.truncated);
    }

    #[tokio::test]
    async fn org_snapshot_reports_assignment_inbox_memory_and_workspace_state() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, _worker) = hierarchy(&engine, &project_id).await;
        let session = engine
            .create_session(project_id.clone(), "org state".into(), None)
            .await
            .expect("session");
        engine
            .assign_session_identity(SessionAssignParams {
                session_id: session.id.clone(),
                identity_id: manager.clone(),
                ..SessionAssignParams::default()
            })
            .await
            .expect("assign");
        engine
            .send_agent_inbox(vp_protocol::types::AgentInboxSendParams {
                project_id: Some(project_id.clone()),
                identity_id: manager.clone(),
                kind: "task".to_string(),
                body: Some("Review the auth worker".to_string()),
                ..vp_protocol::types::AgentInboxSendParams::default()
            })
            .await
            .expect("inbox");
        engine
            .remember_agent_memory(CreateAgentMemoryParams {
                project_id: project_id.clone(),
                identity_id: Some(manager.clone()),
                scope: "agent".to_string(),
                content: "The manager owns the platform review gate.".to_string(),
                evidence: vec![MemoryEvidence {
                    kind: "task".to_string(),
                    reference: "org-test".to_string(),
                    note: None,
                }],
                ..CreateAgentMemoryParams::default()
            })
            .await
            .expect("memory");

        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: project_id.clone(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect("snapshot");
        let node = snapshot
            .leader
            .as_ref()
            .and_then(|leader| find_node(leader, &manager))
            .expect("manager node");
        assert_eq!(
            node.active_assignment
                .as_ref()
                .map(|row| row.session_id.as_str()),
            Some(session.id.as_str())
        );
        assert_eq!(node.unread_inbox, 1);
        assert_eq!(node.memory_count, 1);
        assert_eq!(snapshot.counts.active_assignments, 1);
        assert_eq!(snapshot.counts.unread_inbox, 1);
        assert_eq!(snapshot.counts.memory_count, 1);
        assert_eq!(snapshot.active_assignments.len(), 1);

        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                project_id: Some(project_id.clone()),
                identity_id: manager.clone(),
                include_inbox_content: true,
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert_eq!(detail.identity.id, manager);
        assert_eq!(detail.inbox.len(), 1);
        assert_eq!(detail.inbox[0].status, "unread");
        assert_eq!(
            detail.inbox[0].summary.as_deref(),
            Some("Review the auth worker")
        );
        assert_eq!(detail.inbox_counts.unread, 1);
        assert_eq!(detail.memories.len(), 1);
        assert_eq!(detail.memory_total, 1);
        assert_eq!(detail.performance.assignment_count, 1);
        assert!(detail.workspaces.is_empty());
        assert!(!detail.read_only);
    }

    #[tokio::test]
    async fn inbox_content_is_opt_in_and_never_includes_payload_objects() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, _worker) = hierarchy(&engine, &project_id).await;
        engine
            .send_agent_inbox(vp_protocol::types::AgentInboxSendParams {
                project_id: Some(project_id.clone()),
                identity_id: manager.clone(),
                kind: "message".to_string(),
                payload: Some(serde_json::json!({"body": "one line", "private": "SECRET_PAYLOAD"})),
                ..vp_protocol::types::AgentInboxSendParams::default()
            })
            .await
            .expect("inbox");
        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                project_id: Some(project_id),
                identity_id: manager,
                include_inbox_content: false,
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert_eq!(detail.inbox.len(), 1);
        assert!(detail.inbox[0].summary.is_none());
        let rendered = serde_json::to_string(&detail).expect("json");
        assert!(!rendered.contains("SECRET_PAYLOAD"));
        assert!(!rendered.contains("one line"));
    }

    #[tokio::test]
    async fn imported_and_disabled_states_are_explicit_and_read_only() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        engine
            .store()
            .insert_agent_identity_snapshot(&AgentIdentity {
                id: "imported-1".to_string(),
                project_id: project_id.clone(),
                role: "worker".to_string(),
                title: Some("Imported Worker".to_string()),
                status: "idle".to_string(),
                origin: "imported".to_string(),
                ..AgentIdentity::default()
            })
            .expect("imported");
        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: project_id.clone(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect("snapshot");
        assert!(!snapshot.enabled);
        assert_eq!(snapshot.counts.imported, 1);
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("disabled"))
        );
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("imported"))
        );

        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                identity_id: "imported-1".to_string(),
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert!(detail.read_only);
        assert!(detail.read_only_reason.is_some());
        assert_eq!(detail.identity.origin, "imported");
    }

    #[tokio::test]
    async fn org_snapshot_and_detail_enforce_project_isolation() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, _manager, worker) = hierarchy(&engine, &project_id).await;
        let root = dir.path().join("other");
        std::fs::create_dir_all(&root).expect("repo");
        let other = engine
            .store()
            .add_project(root.to_str().expect("path"), Some("other"))
            .expect("project");
        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: other.id.clone(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect("snapshot");
        assert_eq!(snapshot.counts.identities, 0);
        assert!(snapshot.leader.is_none());

        let error = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                project_id: Some(other.id),
                identity_id: worker,
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect_err("project mismatch");
        let typed = error
            .downcast_ref::<AgentIdentityError>()
            .expect("typed identity error");
        assert_eq!(typed.code, agent_identity_codes::ORG_PROJECT_MISMATCH);
    }

    #[tokio::test]
    async fn retired_identities_are_hidden_by_default_and_kept_as_evidence() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, worker) = hierarchy(&engine, &project_id).await;
        engine
            .retire_agent(vp_protocol::types::AgentRetireParams {
                identity_id: manager.clone(),
                approved: true,
                force: true,
                reason: Some("org test".to_string()),
                ..vp_protocol::types::AgentRetireParams::default()
            })
            .await
            .expect("retire");
        let hidden = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: project_id.clone(),
                include_retired: false,
                limit: None,
            })
            .await
            .expect("hidden");
        assert_eq!(hidden.counts.managers, 0);
        assert!(hidden.leader.as_ref().expect("leader").reports.is_empty());
        assert_eq!(hidden.unassigned.len(), 1);
        assert_eq!(hidden.unassigned[0].identity.id, worker);

        let visible = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: project_id.clone(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect("visible");
        assert_eq!(visible.counts.retired, 1);
        assert_eq!(visible.counts.managers, 1);
    }

    #[tokio::test]
    async fn snapshot_bounds_and_warnings_are_reported() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, _manager, _worker) = hierarchy(&engine, &project_id).await;
        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id,
                include_retired: true,
                limit: Some(1),
            })
            .await
            .expect("snapshot");
        assert!(snapshot.truncated);
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("bounded"))
        );
        assert_eq!(snapshot.counts.identities, 1);
    }

    #[tokio::test]
    async fn detail_uses_real_memory_scope_and_transfer_evidence() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, _worker) = hierarchy(&engine, &project_id).await;
        engine
            .remember_agent_memory(CreateAgentMemoryParams {
                project_id: project_id.clone(),
                scope: "project".to_string(),
                content: "Project-wide fact.".to_string(),
                ..CreateAgentMemoryParams::default()
            })
            .await
            .expect("memory");
        engine
            .remember_agent_memory(CreateAgentMemoryParams {
                project_id: project_id.clone(),
                identity_id: Some(manager.clone()),
                scope: "agent".to_string(),
                content: "Manager fact.".to_string(),
                ..CreateAgentMemoryParams::default()
            })
            .await
            .expect("memory");
        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                identity_id: manager.clone(),
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert_eq!(detail.memories.len(), 2);
        assert_eq!(detail.memory_total, 2);

        engine
            .transfer_agent_identity(AgentTransferParams {
                identity_id: manager.clone(),
                department: Some(Some("Security".to_string())),
                approved: true,
                ..AgentTransferParams::default()
            })
            .await
            .expect("transfer");
        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                identity_id: manager,
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert_eq!(detail.identity.department.as_deref(), Some("Security"));
        assert_eq!(detail.identity.generation, 2);
        assert!(
            detail
                .employment_events
                .iter()
                .any(|event| event.kind == vp_protocol::types::AGENT_TRANSFER_APPROVED_EVENT)
        );
    }

    #[tokio::test]
    async fn pending_hire_requests_are_counted_without_creating_identities() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        enable_identity_policy(&engine, &project_id).await;
        let leader =
            create_identity(&engine, &project_id, "leader", "Leader", None, None, None).await;
        let manager = create_identity(
            &engine,
            &project_id,
            "manager",
            "Platform Manager",
            Some("Platform"),
            Some("platform"),
            Some(&leader),
        )
        .await;
        let result = engine
            .hire_agent_identity(AgentHireParams {
                project_id: project_id.clone(),
                role: "worker".to_string(),
                department: Some("Platform".to_string()),
                component: Some("auth".to_string()),
                title: Some("Auth Worker".to_string()),
                parent_identity_id: Some(manager),
                approved: false,
                ..AgentHireParams::default()
            })
            .await
            .expect("hire request");
        assert_eq!(result.state, "pending");
        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id,
                include_retired: true,
                limit: None,
            })
            .await
            .expect("snapshot");
        assert_eq!(snapshot.counts.identities, 2);
        assert_eq!(snapshot.counts.pending_hire_requests, 1);
        assert_eq!(snapshot.pending_hire_requests.len(), 1);
        assert_eq!(snapshot.counts.workers, 0);
    }

    #[tokio::test]
    async fn retirement_candidates_are_advisory_and_do_not_execute() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, _worker) = hierarchy(&engine, &project_id).await;
        engine
            .set_agent_retirement_policy(vp_protocol::types::SetAgentRetirementPolicyParams {
                project_id: project_id.clone(),
                policy: AgentRetirementPolicy {
                    enabled: true,
                    min_completed_tasks: 1,
                    max_consecutive_failures: 0,
                    ..AgentRetirementPolicy::default()
                },
            })
            .await
            .expect("policy");
        let snapshot = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: project_id.clone(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect("snapshot");
        assert_eq!(snapshot.counts.retirement_candidates, 0);
        let identity = engine.get_agent_identity(&manager).await.expect("identity");
        assert_eq!(identity.identity.status, "active");
        assert_eq!(snapshot.counts.pending_retirement_proposals, 0);
    }

    #[tokio::test]
    async fn detail_respects_bounded_lists_and_reports_truncation() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, _worker) = hierarchy(&engine, &project_id).await;
        for index in 0..4 {
            engine
                .send_agent_inbox(vp_protocol::types::AgentInboxSendParams {
                    project_id: Some(project_id.clone()),
                    identity_id: manager.clone(),
                    kind: "message".to_string(),
                    body: Some(format!("message {index}")),
                    ..vp_protocol::types::AgentInboxSendParams::default()
                })
                .await
                .expect("inbox");
            engine
                .remember_agent_memory(CreateAgentMemoryParams {
                    project_id: project_id.clone(),
                    identity_id: Some(manager.clone()),
                    scope: "agent".to_string(),
                    content: format!("memory {index}"),
                    ..CreateAgentMemoryParams::default()
                })
                .await
                .expect("memory");
        }
        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                identity_id: manager,
                inbox_limit: Some(2),
                memory_limit: Some(2),
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert_eq!(detail.inbox.len(), 2);
        assert_eq!(detail.memories.len(), 2);
        assert!(detail.inbox_counts.unread >= 4);
        assert_eq!(detail.memory_total, 4);
        assert!(detail.truncated);
        assert!(
            detail
                .warnings
                .iter()
                .any(|warning| warning.contains("detail lists"))
        );
    }

    #[tokio::test]
    async fn org_snapshot_rejects_an_unknown_project_with_a_typed_code() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, _project_id) = test_engine(&dir);
        let error = engine
            .agent_org_snapshot(AgentOrgSnapshotParams {
                project_id: "missing".to_string(),
                include_retired: true,
                limit: None,
            })
            .await
            .expect_err("missing project");
        let typed = error
            .downcast_ref::<AgentIdentityError>()
            .expect("typed identity error");
        assert_eq!(typed.code, agent_identity_codes::ORG_PROJECT_NOT_FOUND);
    }

    #[tokio::test]
    async fn memory_visibility_for_other_identity_is_enforced() {
        let dir = TempDir::new().expect("tempdir");
        let (engine, project_id) = test_engine(&dir);
        let (_leader, manager, worker) = hierarchy(&engine, &project_id).await;
        engine
            .remember_agent_memory(CreateAgentMemoryParams {
                project_id: project_id.clone(),
                identity_id: Some(manager.clone()),
                scope: "agent".to_string(),
                content: "Private manager memory.".to_string(),
                ..CreateAgentMemoryParams::default()
            })
            .await
            .expect("memory");
        let detail = engine
            .agent_identity_detail(AgentIdentityDetailParams {
                identity_id: worker,
                ..AgentIdentityDetailParams::default()
            })
            .await
            .expect("detail");
        assert!(
            detail
                .memories
                .iter()
                .all(|memory| memory.content != "Private manager memory.")
        );
    }
}

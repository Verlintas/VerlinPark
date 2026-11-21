use super::*;
use tempfile::TempDir;
use vp_protocol::types::{
    AgentHireParams, AgentIdentityPolicy, AgentPromoteParams, AgentRetireParams,
    AgentRetirementPolicy, AgentTransferParams, AgentWorkspace, CreateAgentIdentityParams, Project,
    Session,
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

fn active_workspace(store: &Store, project: &Project, identity: &AgentIdentity) -> AgentWorkspace {
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
        contracts: Vec::new(),
        contract_usage: Vec::new(),
        progress: Vec::new(),
        remote_test_evidence: Vec::new(),
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
        team_templates: Vec::new(),
        project_team_templates: Vec::new(),
        team_template_events: Vec::new(),
        session_auto_budget: None,
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
        imported_requests
            .iter()
            .any(|request| { request.id == pending.request.id && request.origin == "imported" })
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

//! S2 session assignment, identity lease, project inbox, and wake tests.
//!
//! These use the same Store + Engine APIs as the daemon and never call a
//! provider. A "wake" is asserted to be a persisted deferred continuation, not
//! a run: no agent row, Worker slot, or provider request is created.

use tempfile::TempDir;
use time::OffsetDateTime;
use vp_core::Engine;
use vp_core::rebuild::rebuild_database;
use vp_protocol::types::{
    AgentCoordinationError, AgentCoordinationReplayParams, AgentIdentityPolicy,
    AgentInboxListParams, AgentInboxMessageParams, AgentInboxSendParams, CreateAgentIdentityParams,
    Project, RetireAgentIdentityParams, Session, SessionAssignParams, SessionAssignmentsParams,
    SessionReleaseParams, agent_coordination_codes, agent_identity_codes,
};
use vp_store::Store;

struct Harness {
    data: TempDir,
    _repo: TempDir,
    engine: Engine,
    project: Project,
}

fn harness(name: &str) -> Harness {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some(name))
        .unwrap();
    Harness {
        data,
        _repo: repo,
        engine: Engine::new(store),
        project,
    }
}

async fn enable_policy(engine: &Engine, project_id: &str) {
    engine
        .set_agent_identity_policy(
            project_id,
            AgentIdentityPolicy {
                enabled: true,
                ..AgentIdentityPolicy::default()
            },
        )
        .await
        .unwrap();
}

async fn identity(engine: &Engine, project_id: &str, role: &str) -> String {
    engine
        .create_agent_identity(CreateAgentIdentityParams {
            project_id: project_id.to_string(),
            role: role.to_string(),
            title: Some(format!("{role} title")),
            ..CreateAgentIdentityParams::default()
        })
        .await
        .unwrap()
        .identity
        .id
}

fn session(store: &Store, project_id: &str, goal: &str) -> Session {
    store.create_session(project_id, goal).unwrap()
}

fn coordination_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<AgentCoordinationError>()
        .map(|typed| typed.code)
        .unwrap_or("UNTYPED")
}

fn identity_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<vp_protocol::types::AgentIdentityError>()
        .map(|typed| typed.code)
        .unwrap_or("UNTYPED")
}

fn assign_params(session_id: &str, identity_id: &str) -> SessionAssignParams {
    SessionAssignParams {
        session_id: session_id.to_string(),
        identity_id: identity_id.to_string(),
        ..SessionAssignParams::default()
    }
}

#[tokio::test]
async fn assignment_is_unique_idempotent_and_project_scoped() {
    let harness = harness("assignment");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "worker").await;
    let session_a = session(engine.store(), &project.id, "a");
    let session_b = session(engine.store(), &project.id, "b");

    let created = engine
        .assign_session_identity(assign_params(&session_a.id, &worker))
        .await
        .unwrap();
    assert!(created.created);
    assert_eq!(created.assignment.status, "active");
    assert!(created.assignment.lease_active);
    assert_eq!(created.assignment.project_id, project.id);
    assert_eq!(created.assignment.session_id, session_a.id);

    let idempotent = engine
        .assign_session_identity(assign_params(&session_a.id, &worker))
        .await
        .unwrap();
    assert!(!idempotent.created);
    assert_eq!(
        idempotent.assignment.assigned_at,
        created.assignment.assigned_at
    );

    let conflict = engine
        .assign_session_identity(assign_params(&session_b.id, &worker))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&conflict),
        agent_coordination_codes::ASSIGNMENT_CONFLICT
    );

    let by_session = engine
        .session_assignments(SessionAssignmentsParams {
            session_id: Some(session_a.id.clone()),
            ..SessionAssignmentsParams::default()
        })
        .await
        .unwrap();
    assert_eq!(by_session.assignments.len(), 1);
    let by_identity = engine
        .session_assignments(SessionAssignmentsParams {
            identity_id: Some(worker.clone()),
            ..SessionAssignmentsParams::default()
        })
        .await
        .unwrap();
    assert_eq!(by_identity.assignments.len(), 1);
    let by_project = engine
        .session_assignments(SessionAssignmentsParams {
            project_id: Some(project.id.clone()),
            ..SessionAssignmentsParams::default()
        })
        .await
        .unwrap();
    assert_eq!(by_project.assignments.len(), 1);

    let other_repo = TempDir::new().unwrap();
    let other_project = engine
        .store()
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    let other_worker = identity(engine, &other_project.id, "worker").await;
    let cross_project = engine
        .assign_session_identity(assign_params(&session_a.id, &other_worker))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&cross_project),
        agent_coordination_codes::ASSIGNMENT_PROJECT_MISMATCH
    );

    let role_mismatch = engine
        .assign_session_identity(SessionAssignParams {
            role: Some("manager".into()),
            ..assign_params(&session_a.id, &worker)
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&role_mismatch),
        agent_coordination_codes::ASSIGNMENT_ROLE_MISMATCH
    );

    let imported_session = session(engine.store(), &project.id, "imported");
    engine
        .store()
        .update_session_status(&imported_session.id, "imported")
        .unwrap();
    let imported = engine
        .assign_session_identity(assign_params(&imported_session.id, &worker))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&imported),
        agent_coordination_codes::ASSIGNMENT_INVALID
    );

    let released = engine
        .release_session_identity(SessionReleaseParams {
            session_id: session_a.id.clone(),
            identity_id: worker.clone(),
            operator: true,
            reason: Some("done".into()),
        })
        .await
        .unwrap();
    assert!(released.released);
    assert_eq!(released.assignment.status, "released");
    assert!(released.assignment.lease_expires_at.is_none());
    let again = engine
        .release_session_identity(SessionReleaseParams {
            session_id: session_a.id.clone(),
            identity_id: worker.clone(),
            ..SessionReleaseParams::default()
        })
        .await
        .unwrap();
    assert!(!again.released);

    let wrong_owner = engine
        .release_session_identity(SessionReleaseParams {
            session_id: session_b.id.clone(),
            identity_id: worker.clone(),
            ..SessionReleaseParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&wrong_owner),
        agent_coordination_codes::ASSIGNMENT_NOT_FOUND
    );

    let reassigned = engine
        .assign_session_identity(assign_params(&session_b.id, &worker))
        .await
        .unwrap();
    assert!(reassigned.created);
    assert_eq!(reassigned.assignment.session_id, session_b.id);
}

#[tokio::test]
async fn paused_retired_and_imported_identities_cannot_be_assigned() {
    let harness = harness("unavailable");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "worker").await;
    let session = session(engine.store(), &project.id, "session");
    engine
        .pause_agent_identity(vp_protocol::types::PauseAgentIdentityParams {
            identity_id: worker.clone(),
        })
        .await
        .unwrap();
    let paused = engine
        .assign_session_identity(assign_params(&session.id, &worker))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&paused),
        agent_coordination_codes::ASSIGNMENT_IDENTITY_UNAVAILABLE
    );
    engine
        .resume_agent_identity(vp_protocol::types::ResumeAgentIdentityParams {
            identity_id: worker.clone(),
        })
        .await
        .unwrap();
    engine
        .retire_agent_identity(RetireAgentIdentityParams {
            identity_id: worker.clone(),
            successor_id: None,
        })
        .await
        .unwrap();
    let retired = engine
        .assign_session_identity(assign_params(&session.id, &worker))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&retired),
        agent_coordination_codes::ASSIGNMENT_IDENTITY_RETIRED
    );

    // Imported identities are read-only until an explicit rehome. Rebuild the
    // project projection with one imported row to exercise that guard.
    let mut identities = engine
        .store()
        .list_agent_identities(&project.id, None, None)
        .unwrap();
    identities[0].origin = "imported".into();
    engine
        .store()
        .replace_agent_identity_projection(&project.id, &identities)
        .unwrap();
    let imported = engine
        .assign_session_identity(assign_params(&session.id, &identities[0].id))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&imported),
        agent_coordination_codes::ASSIGNMENT_IDENTITY_UNAVAILABLE
    );
}

#[tokio::test]
async fn policy_off_and_unsupported_identity_roles_fail_closed() {
    let harness = harness("policy");
    let engine = &harness.engine;
    let project = &harness.project;
    let worker = identity(engine, &project.id, "worker").await;
    let session = session(engine.store(), &project.id, "session");
    let disabled = engine
        .assign_session_identity(assign_params(&session.id, &worker))
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&disabled),
        agent_coordination_codes::ASSIGNMENT_POLICY_DISABLED
    );

    for role in ["labour", "verifier", "scout"] {
        let error = engine
            .create_agent_identity(CreateAgentIdentityParams {
                project_id: project.id.clone(),
                role: role.into(),
                ..CreateAgentIdentityParams::default()
            })
            .await
            .unwrap_err();
        assert_eq!(
            identity_code(&error),
            agent_identity_codes::IDENTITY_ROLE_INVALID,
            "role {role} must not be a project identity"
        );
    }
}

#[tokio::test]
async fn expired_leases_are_reclaimed_and_reconciliation_survives_restart() {
    let harness = harness("expiry");
    let project_id = harness.project.id.clone();
    let data_path = harness.data.path().to_path_buf();
    let (session_a_id, session_b_id, worker) = {
        let engine = &harness.engine;
        enable_policy(engine, &project_id).await;
        let worker = identity(engine, &project_id, "worker").await;
        let session_a = session(engine.store(), &project_id, "a");
        let session_b = session(engine.store(), &project_id, "b");
        engine
            .assign_session_identity(SessionAssignParams {
                lease_secs: Some(60),
                ..assign_params(&session_a.id, &worker)
            })
            .await
            .unwrap();
        (session_a.id, session_b.id, worker)
    };

    // No sweep before the lease is due.
    let early = harness
        .engine
        .store()
        .expire_session_agent_assignments_at(OffsetDateTime::now_utc(), None)
        .unwrap();
    assert!(early.is_empty());

    // Close the first engine and reopen the same data directory: the startup
    // reconciliation path is the same public store operation.
    drop(harness.engine);
    let store = Store::open(&data_path).unwrap();
    let now = OffsetDateTime::now_utc() + time::Duration::seconds(61);
    let events = store
        .expire_session_agent_assignments_at(now, None)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "agent.assignment.expired");
    let released = store
        .get_session_agent_assignment(&session_a_id, &worker)
        .unwrap()
        .unwrap();
    assert_eq!(released.status, "released");
    assert!(!released.lease_active);
    let identities = store
        .list_agent_identities(&project_id, None, None)
        .unwrap();
    assert_eq!(identities.len(), 1);

    let engine = Engine::new(store);
    let reassigned = engine
        .assign_session_identity(assign_params(&session_b_id, &worker))
        .await
        .unwrap();
    assert!(reassigned.created);
    assert_eq!(reassigned.assignment.session_id, session_b_id);

    // The explicit renewal path cannot revive the old lease.
    let error = engine
        .renew_session_assignment(vp_protocol::types::SessionAssignmentRenewParams {
            session_id: session_a_id,
            identity_id: worker,
            ..vp_protocol::types::SessionAssignmentRenewParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&error),
        agent_coordination_codes::ASSIGNMENT_LEASE_EXPIRED
    );
}

#[tokio::test]
async fn inbox_lifecycle_counts_and_cross_project_rejections() {
    let harness = harness("inbox");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "worker").await;
    let manager = identity(engine, &project.id, "manager").await;
    let session = session(engine.store(), &project.id, "inbox");

    let sent = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker.clone(),
            session_id: Some(session.id.clone()),
            from_identity_id: Some(manager.clone()),
            kind: "handoff".into(),
            body: Some("review the diff".into()),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap();
    assert_eq!(sent.message.status, "unread");
    assert_eq!(sent.message.from_label, manager);
    assert!(!sent.wake.requested);

    let listed = engine
        .list_agent_inbox(AgentInboxListParams {
            project_id: Some(project.id.clone()),
            identity_id: Some(worker.clone()),
            unread_only: true,
            ..AgentInboxListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.messages.len(), 1);
    assert_eq!(listed.counts.unread, 1);

    let read = engine
        .read_agent_inbox(AgentInboxMessageParams {
            message_id: sent.message.id.clone(),
            project_id: Some(project.id.clone()),
            identity_id: Some(worker.clone()),
        })
        .await
        .unwrap();
    assert!(read.changed);
    assert_eq!(read.message.status, "read");
    let read_again = engine
        .read_agent_inbox(AgentInboxMessageParams {
            message_id: sent.message.id.clone(),
            ..AgentInboxMessageParams::default()
        })
        .await
        .unwrap();
    assert!(!read_again.changed);

    let done = engine
        .ack_agent_inbox(AgentInboxMessageParams {
            message_id: sent.message.id.clone(),
            project_id: Some(project.id.clone()),
            identity_id: Some(worker.clone()),
        })
        .await
        .unwrap();
    assert!(done.changed);
    assert_eq!(done.message.status, "done");
    let ack_again = engine
        .ack_agent_inbox(AgentInboxMessageParams {
            message_id: sent.message.id.clone(),
            ..AgentInboxMessageParams::default()
        })
        .await
        .unwrap();
    assert!(!ack_again.changed);

    let other_repo = TempDir::new().unwrap();
    let other_project = engine
        .store()
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    let cross_project = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(other_project.id.clone()),
            identity_id: worker.clone(),
            kind: "message".into(),
            body: Some("cross".into()),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&cross_project),
        agent_coordination_codes::INBOX_PROJECT_MISMATCH
    );

    let other_identity = identity(engine, &other_project.id, "worker").await;
    let cross_sender = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker.clone(),
            from_identity_id: Some(other_identity),
            kind: "message".into(),
            body: Some("cross sender".into()),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&cross_sender),
        agent_coordination_codes::INBOX_SENDER_INVALID
    );

    let retired = identity(engine, &project.id, "worker").await;
    engine
        .retire_agent_identity(RetireAgentIdentityParams {
            identity_id: retired.clone(),
            successor_id: None,
        })
        .await
        .unwrap();
    let retired_error = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: retired,
            kind: "message".into(),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&retired_error),
        agent_coordination_codes::INBOX_RECIPIENT_RETIRED
    );

    let secret_payload = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker,
            kind: "message".into(),
            payload: Some(serde_json::json!({"api_key": "must-not-persist"})),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&secret_payload),
        agent_coordination_codes::INBOX_PAYLOAD_INVALID
    );
}

#[tokio::test]
async fn inbox_ttl_read_fails_closed_and_sweep_expires_rows() {
    let harness = harness("ttl");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "worker").await;
    let message = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker.clone(),
            kind: "message".into(),
            body: Some("short lived".into()),
            expires_in_secs: Some(60),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap();
    let store = engine.store();
    let mut row = store
        .get_agent_inbox_message(&message.message.id)
        .unwrap()
        .unwrap();
    row.expires_at = Some("2000-01-01T00:00:00Z".into());
    store
        .replace_agent_coordination_projection(
            &project.id,
            &store
                .list_agent_assignments_for_replay(&project.id)
                .unwrap(),
            &[row.clone()],
        )
        .unwrap();
    let error = engine
        .read_agent_inbox(AgentInboxMessageParams {
            message_id: row.id.clone(),
            ..AgentInboxMessageParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        coordination_code(&error),
        agent_coordination_codes::INBOX_EXPIRED
    );
    assert_eq!(
        store
            .get_agent_inbox_message(&row.id)
            .unwrap()
            .unwrap()
            .status,
        "expired"
    );

    let second = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker,
            kind: "decision".into(),
            body: Some("sweep me".into()),
            expires_in_secs: Some(60),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap();
    let mut second_row = store
        .get_agent_inbox_message(&second.message.id)
        .unwrap()
        .unwrap();
    second_row.expires_at = Some("2000-01-01T00:00:00Z".into());
    store
        .replace_agent_coordination_projection(
            &project.id,
            &store
                .list_agent_assignments_for_replay(&project.id)
                .unwrap(),
            &[
                store.get_agent_inbox_message(&row.id).unwrap().unwrap(),
                second_row,
            ],
        )
        .unwrap();
    let expired = engine.sweep_agent_inbox_expiry().await.unwrap();
    assert_eq!(expired, 1);
}

#[tokio::test]
async fn wake_is_an_idempotent_deferred_continuation_without_a_slot() {
    let harness = harness("wake");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "worker").await;
    let session = session(engine.store(), &project.id, "wake");
    engine
        .store()
        .update_session_status(&session.id, "idle")
        .unwrap();

    let first = engine
        .assign_session_identity(SessionAssignParams {
            wake_session: true,
            ..assign_params(&session.id, &worker)
        })
        .await
        .unwrap();
    assert!(first.wake.requested);
    assert!(first.wake.created, "{:?}", first.wake);
    assert!(first.wake.continuation_id.is_some());
    let continuations = engine
        .store()
        .list_session_continuations(&session.id, None, None)
        .unwrap();
    assert_eq!(continuations.len(), 1);
    assert!(continuations[0].body.starts_with("[inbox] assignment:"));
    assert!(continuations[0].body.contains("cooperative only"));
    assert!(engine.store().list_agents(&session.id).unwrap().is_empty());
    assert_eq!(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .status,
        "idle"
    );

    let second = engine
        .assign_session_identity(SessionAssignParams {
            wake_session: true,
            ..assign_params(&session.id, &worker)
        })
        .await
        .unwrap();
    assert!(!second.created);
    assert!(second.wake.created);
    assert_eq!(
        second.wake.continuation_id, first.wake.continuation_id,
        "the operation receipt must replay the same continuation"
    );
    assert_eq!(
        engine
            .store()
            .list_session_continuations(&session.id, None, None)
            .unwrap()
            .len(),
        1
    );

    engine
        .store()
        .update_session_status(&session.id, "running")
        .unwrap();
    let busy = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker.clone(),
            session_id: Some(session.id.clone()),
            kind: "message".into(),
            body: Some("do not interrupt".into()),
            wake_session: true,
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap();
    assert!(busy.wake.requested);
    assert!(!busy.wake.created);
    assert_eq!(busy.wake.reason, "session_running");
    assert_eq!(busy.message.status, "unread");
    assert_eq!(
        engine
            .store()
            .list_session_continuations(&session.id, None, None)
            .unwrap()
            .len(),
        1
    );

    engine
        .store()
        .update_session_status(&session.id, "idle")
        .unwrap();
    let woke = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker,
            kind: "message".into(),
            body: Some("please review".into()),
            wake_session: true,
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap();
    assert!(woke.wake.created, "{:?}", woke.wake);
    let continuations = engine
        .store()
        .list_session_continuations(&session.id, None, None)
        .unwrap();
    assert_eq!(continuations.len(), 2);
    assert!(
        continuations
            .iter()
            .any(|continuation| continuation.body.starts_with("[inbox]"))
    );
    assert!(engine.store().list_agents(&session.id).unwrap().is_empty());
}

#[tokio::test]
async fn replay_repair_manifest_rebuild_and_bundle_preserve_coordination() {
    let harness = harness("replay");
    let engine = &harness.engine;
    let project = &harness.project;
    enable_policy(engine, &project.id).await;
    let worker = identity(engine, &project.id, "worker").await;
    let session = session(engine.store(), &project.id, "replay");
    engine
        .append_event(
            &session.id,
            "session.created",
            serde_json::json!({
                "project_id": project.id,
                "goal": session.goal,
                "created_at": session.created_at,
            }),
        )
        .await
        .unwrap();
    engine
        .assign_session_identity(assign_params(&session.id, &worker))
        .await
        .unwrap();
    let sent = engine
        .send_agent_inbox(AgentInboxSendParams {
            project_id: Some(project.id.clone()),
            identity_id: worker.clone(),
            session_id: Some(session.id.clone()),
            kind: "review".into(),
            body: Some("review the archive".into()),
            ..AgentInboxSendParams::default()
        })
        .await
        .unwrap();

    let replay = engine
        .replay_agent_coordination(AgentCoordinationReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(replay.consistent, "{replay:?}");
    assert_eq!(replay.assignments, 1);
    assert_eq!(replay.inbox, 1);

    engine
        .store()
        .replace_agent_coordination_projection(&project.id, &[], &[])
        .unwrap();
    let drift = engine
        .replay_agent_coordination(AgentCoordinationReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(!drift.consistent);
    let repaired = engine
        .replay_agent_coordination(AgentCoordinationReplayParams {
            project_id: project.id.clone(),
            repair: true,
        })
        .await
        .unwrap();
    assert!(repaired.repaired);
    assert!(repaired.consistent, "{repaired:?}");
    assert_eq!(
        engine
            .store()
            .get_agent_inbox_message(&sent.message.id)
            .unwrap()
            .unwrap()
            .status,
        "unread"
    );

    let events_before = engine
        .store()
        .list_agent_coordination_events_for_project(&project.id, None)
        .unwrap()
        .len();
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.session_agent_assignments.len(), 1);
    assert_eq!(bundle.agent_inbox.len(), 1);
    assert_eq!(bundle.agent_coordination_events.len(), events_before);
    let imported_data = TempDir::new().unwrap();
    let imported_store = Store::open(imported_data.path()).unwrap();
    imported_store.import_session_bundle(&bundle).unwrap();
    assert_eq!(
        imported_store
            .get_session_agent_assignment(&session.id, &worker)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    assert_eq!(
        imported_store
            .get_agent_inbox_message(&sent.message.id)
            .unwrap()
            .unwrap()
            .status,
        "unread"
    );
    assert_eq!(
        imported_store
            .list_agent_coordination_events_for_project(&project.id, None)
            .unwrap()
            .len(),
        events_before
    );

    engine.record_project_manifest().await.unwrap();
    let data_path = harness.data.path().to_path_buf();
    drop(harness.engine);
    let report = rebuild_database(&data_path, false).await.unwrap();
    assert_eq!(report.projects, 1);
    assert_eq!(report.sessions, 1, "{:?}", report.skipped);
    let store = Store::open(&data_path).unwrap();
    let assignments = store
        .list_agent_assignments_for_replay(&project.id)
        .unwrap();
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].status, "active");
    let inbox = store.list_agent_inbox_for_replay(&project.id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, "unread");
    let events = store
        .list_agent_coordination_events_for_project(&project.id, None)
        .unwrap();
    assert_eq!(events.len(), events_before);
    let engine = Engine::new(store);
    let replay = engine
        .replay_agent_coordination(AgentCoordinationReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(replay.consistent, "{replay:?}");
}

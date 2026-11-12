//! S1 project-scoped identity lifecycle, policy, replay, rebuild, and archive
//! tests. These use the same Store + Engine APIs as the daemon and never call a
//! provider.

use tempfile::TempDir;
use vp_core::Engine;
use vp_core::rebuild::rebuild_database;
use vp_protocol::types::{
    AgentIdentityError, AgentIdentityPolicy, AgentIdentityReplayParams, CreateAgentIdentityParams,
    PauseAgentIdentityParams, Project, ResumeAgentIdentityParams, RetireAgentIdentityParams,
    UpdateAgentIdentityParams, agent_identity_codes,
};
use vp_store::Store;

fn harness(name: &str) -> (TempDir, TempDir, Engine, Project) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some(name))
        .unwrap();
    (data, repo, Engine::new(store), project)
}

fn params(project_id: &str, role: &str, parent: Option<&str>) -> CreateAgentIdentityParams {
    CreateAgentIdentityParams {
        project_id: project_id.to_string(),
        role: role.to_string(),
        department: Some("engineering".to_string()),
        component: Some("runtime".to_string()),
        title: Some(format!("{role} title")),
        profile: Some("coding".to_string()),
        model: Some("explicit-model".to_string()),
        parent_identity_id: parent.map(str::to_string),
        policy: Some(serde_json::json!({"note": "no secrets"})),
    }
}

fn typed_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<AgentIdentityError>()
        .map(|typed| typed.code)
        .unwrap_or("UNTYPED")
}

#[tokio::test]
async fn lifecycle_replay_and_repair_are_event_backed() {
    let (_data, _repo, engine, project) = harness("lifecycle");
    let leader = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap();
    assert!(leader.changed);
    assert_eq!(leader.identity.generation, 1);
    let manager = engine
        .create_agent_identity(params(&project.id, "manager", Some(&leader.identity.id)))
        .await
        .unwrap();

    let updated = engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
            department: Some(Some("platform".to_string())),
            title: Some(Some("Platform Lead".to_string())),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    assert!(updated.changed);
    assert_eq!(updated.identity.id, manager.identity.id);
    assert_eq!(updated.identity.generation, 2);
    assert_eq!(updated.identity.department.as_deref(), Some("platform"));

    let paused = engine
        .pause_agent_identity(PauseAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
        })
        .await
        .unwrap();
    assert!(paused.changed);
    assert_eq!(paused.identity.status, "paused");
    assert_eq!(paused.identity.generation, 2);
    let resumed = engine
        .resume_agent_identity(ResumeAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(resumed.identity.status, "active");
    let retired = engine
        .retire_agent_identity(RetireAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
            successor_id: Some(leader.identity.id.clone()),
        })
        .await
        .unwrap();
    assert_eq!(retired.identity.status, "retired");
    assert!(retired.identity.retired_at.is_some());

    let listed = engine
        .list_agent_identities_for_project(&project.id, None, None)
        .await
        .unwrap();
    assert_eq!(listed.identities.len(), 2);
    assert_eq!(
        engine
            .list_agent_identities_for_project(&project.id, Some("manager"), Some("retired"))
            .await
            .unwrap()
            .identities
            .len(),
        1
    );

    let events = engine
        .store()
        .list_agent_identity_events_for_project(&project.id, None)
        .unwrap();
    assert!(events.len() >= 6);
    let replay = engine
        .replay_agent_identities(AgentIdentityReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(replay.consistent, "{replay:?}");
    assert_eq!(replay.identities, 2);
    assert_eq!(replay.events as usize, events.len());

    let mut corrupt = listed.identities.clone();
    for identity in &mut corrupt {
        if identity.id == manager.identity.id {
            identity.generation = 99;
        }
    }
    engine
        .store()
        .replace_agent_identity_projection(&project.id, &corrupt)
        .unwrap();
    let drift = engine
        .replay_agent_identities(AgentIdentityReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(!drift.consistent);
    assert!(
        drift
            .mismatches
            .iter()
            .any(|line| line.contains("generation"))
    );
    let repaired = engine
        .replay_agent_identities(AgentIdentityReplayParams {
            project_id: project.id.clone(),
            repair: true,
        })
        .await
        .unwrap();
    assert!(repaired.repaired);
    assert!(repaired.consistent, "{repaired:?}");
    let events_after = engine
        .store()
        .list_agent_identity_events_for_project(&project.id, None)
        .unwrap();
    assert_eq!(events.len(), events_after.len());

    let missing = engine
        .get_agent_identity("missing-identity")
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&missing),
        agent_identity_codes::IDENTITY_NOT_FOUND
    );
}

#[tokio::test]
async fn policy_defaults_validation_and_project_isolation() {
    let (_data, _repo, engine, project) = harness("policy");
    let other = engine
        .store()
        .add_project(
            TempDir::new().unwrap().path().to_str().unwrap(),
            Some("other"),
        )
        .unwrap();
    let policy = engine.get_agent_identity_policy(&project.id).await.unwrap();
    assert!(!policy.enabled);
    assert_eq!(policy.max_managers, 4);
    assert_eq!(policy.max_workers, 10);
    assert!(policy.hire_requires_approval);
    assert!(policy.retirement_requires_approval);

    let invalid = AgentIdentityPolicy {
        max_managers: 9,
        ..AgentIdentityPolicy::default()
    };
    let error = engine
        .set_agent_identity_policy(&project.id, invalid)
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_POLICY_INVALID
    );

    let set = engine
        .set_agent_identity_policy(
            &project.id,
            AgentIdentityPolicy {
                enabled: true,
                max_managers: 1,
                max_workers: 1,
                hire_requires_approval: false,
                retirement_requires_approval: true,
            },
        )
        .await
        .unwrap();
    assert!(set.enabled);
    let other_policy = engine.get_agent_identity_policy(&other.id).await.unwrap();
    assert!(!other_policy.enabled);
    let result = engine
        .list_agent_identities_for_project(&project.id, Some("worker"), Some("active"))
        .await
        .unwrap();
    assert!(result.identities.is_empty());
    let error = engine
        .list_agent_identities_for_project(&project.id, Some("labour"), None)
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_ROLE_INVALID
    );
}

#[tokio::test]
async fn parent_successor_and_roles_are_project_scoped() {
    let (_data, _repo, engine, project_a) = harness("scope-a");
    let project_b = engine
        .store()
        .add_project(
            TempDir::new().unwrap().path().to_str().unwrap(),
            Some("scope-b"),
        )
        .unwrap();
    let leader_a = engine
        .create_agent_identity(params(&project_a.id, "leader", None))
        .await
        .unwrap();
    let leader_b = engine
        .create_agent_identity(params(&project_b.id, "leader", None))
        .await
        .unwrap();

    let error = engine
        .create_agent_identity(params(
            &project_a.id,
            "manager",
            Some(&leader_b.identity.id),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_PROJECT_MISMATCH
    );
    let manager = engine
        .create_agent_identity(params(
            &project_a.id,
            "manager",
            Some(&leader_a.identity.id),
        ))
        .await
        .unwrap();
    let error = engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
            successor_id: Some(Some(leader_b.identity.id.clone())),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_PROJECT_MISMATCH
    );
    let error = engine
        .create_agent_identity(params(&project_a.id, "labour", None))
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_ROLE_INVALID
    );
    let error = engine
        .create_agent_identity(params(&project_a.id, "verifier", None))
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_ROLE_INVALID
    );
    let error = engine
        .create_agent_identity(params(&project_a.id, "scout", None))
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_ROLE_INVALID
    );
    let error = engine
        .create_agent_identity(params(&project_a.id, "worker", Some(&leader_a.identity.id)))
        .await
        .unwrap_err();
    assert_eq!(typed_code(&error), agent_identity_codes::IDENTITY_INVALID);
}

#[tokio::test]
async fn caps_retirement_and_imported_read_only_are_typed() {
    let (_data, _repo, engine, project) = harness("caps");
    engine
        .set_agent_identity_policy(
            &project.id,
            AgentIdentityPolicy {
                enabled: true,
                max_managers: 1,
                max_workers: 1,
                ..AgentIdentityPolicy::default()
            },
        )
        .await
        .unwrap();
    let leader = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap();
    let manager = engine
        .create_agent_identity(params(&project.id, "manager", Some(&leader.identity.id)))
        .await
        .unwrap();
    let error = engine
        .create_agent_identity(params(&project.id, "manager", Some(&leader.identity.id)))
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_CAP_EXCEEDED
    );
    let worker = engine
        .create_agent_identity(params(&project.id, "worker", Some(&manager.identity.id)))
        .await
        .unwrap();
    let error = engine
        .create_agent_identity(params(&project.id, "worker", Some(&manager.identity.id)))
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_CAP_EXCEEDED
    );
    engine
        .retire_agent_identity(RetireAgentIdentityParams {
            identity_id: worker.identity.id.clone(),
            successor_id: None,
        })
        .await
        .unwrap();
    engine
        .create_agent_identity(params(&project.id, "worker", Some(&manager.identity.id)))
        .await
        .unwrap();

    let error = engine
        .resume_agent_identity(ResumeAgentIdentityParams {
            identity_id: worker.identity.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(typed_code(&error), agent_identity_codes::IDENTITY_RETIRED);

    let mut imported = leader.identity.clone();
    imported.id = "imported-leader".to_string();
    imported.origin = "imported".to_string();
    imported.status = "active".to_string();
    engine
        .store()
        .insert_agent_identity_snapshot(&imported)
        .unwrap();
    let error = engine
        .pause_agent_identity(PauseAgentIdentityParams {
            identity_id: imported.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_IMPORTED_READONLY
    );
}

#[tokio::test]
async fn policy_disabled_leaves_existing_sessions_and_runs_unchanged() {
    let (_data, _repo, engine, project) = harness("disabled");
    let session = engine
        .store()
        .create_session(&project.id, "legacy session")
        .unwrap();
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
    let (agent, _event) = engine
        .store()
        .create_leader_agent_with_snapshot(&session.id, "legacy-model")
        .unwrap();
    let before_sessions = engine.store().list_sessions(None).unwrap();
    let before_agents = engine.store().list_agents(&session.id).unwrap();
    assert_eq!(before_agents.len(), 1);
    assert!(before_agents[0].identity_id.is_none());

    // Creating identities is explicit and does not touch the run roster.
    let leader = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap();
    assert_eq!(leader.identity.status, "active");
    let after_sessions = engine.store().list_sessions(None).unwrap();
    assert_eq!(before_sessions.len(), after_sessions.len());
    assert_eq!(before_sessions[0].status, after_sessions[0].status);
    let after_agents = engine.store().list_agents(&session.id).unwrap();
    assert_eq!(before_agents[0].id, after_agents[0].id);
    assert!(after_agents[0].identity_id.is_none());
    assert_eq!(after_agents[0].id, agent.id);

    // No session assignment, inbox, workspace, or memory behavior is created.
    assert!(
        engine
            .store()
            .list_agent_messages(&session.id, None, false)
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .store()
            .list_artifacts(&session.id)
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .store()
            .list_memories(&project.id, None, None, 100)
            .unwrap()
            .is_empty()
    );

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{report:?}");
}

#[tokio::test]
async fn manifest_rebuild_restores_policy_identities_and_events() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let project_id;
    let identity_id;
    let event_count;
    {
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let project = engine
            .store()
            .add_project(repo.path().to_str().unwrap(), Some("rebuild"))
            .unwrap();
        project_id = project.id.clone();
        engine
            .set_agent_identity_policy(
                &project.id,
                AgentIdentityPolicy {
                    enabled: true,
                    max_managers: 2,
                    max_workers: 3,
                    ..AgentIdentityPolicy::default()
                },
            )
            .await
            .unwrap();
        let leader = engine
            .create_agent_identity(params(&project.id, "leader", None))
            .await
            .unwrap();
        identity_id = leader.identity.id.clone();
        let manager = engine
            .create_agent_identity(params(&project.id, "manager", Some(&leader.identity.id)))
            .await
            .unwrap();
        engine
            .update_agent_identity(UpdateAgentIdentityParams {
                identity_id: manager.identity.id.clone(),
                component: Some(Some("platform".to_string())),
                ..UpdateAgentIdentityParams::default()
            })
            .await
            .unwrap();
        event_count = engine
            .store()
            .list_agent_identity_events_for_project(&project.id, None)
            .unwrap()
            .len();
        engine.record_project_manifest().await.unwrap();
    }

    let report = rebuild_database(data.path(), false).await.unwrap();
    assert_eq!(report.projects, 1);
    let store = Store::open(data.path()).unwrap();
    let policy = store.get_agent_identity_policy(&project_id).unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.max_managers, 2);
    let identity = store.get_agent_identity(&identity_id).unwrap().unwrap();
    assert_eq!(identity.role, "leader");
    assert_eq!(identity.project_id, project_id);
    let events = store
        .list_agent_identity_events_for_project(&project_id, None)
        .unwrap();
    assert_eq!(events.len(), event_count);
    let identities = store
        .list_agent_identities(&project_id, None, None)
        .unwrap();
    assert_eq!(identities.len(), 2);
    let manager = identities
        .iter()
        .find(|identity| identity.role == "manager")
        .unwrap();
    assert_eq!(manager.component.as_deref(), Some("platform"));
    assert_eq!(manager.generation, 2);
}

#[tokio::test]
async fn bundle_export_import_preserves_identity_scope_and_read_only_origin() {
    let (_data, repo, engine, project) = harness("bundle");
    let session = engine
        .store()
        .create_session(&project.id, "archive identity")
        .unwrap();
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
    let (agent, _) = engine
        .store()
        .create_leader_agent_with_snapshot(&session.id, "legacy-model")
        .unwrap();
    let leader = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap();
    let manager = engine
        .create_agent_identity(params(&project.id, "manager", Some(&leader.identity.id)))
        .await
        .unwrap();
    engine
        .pause_agent_identity(PauseAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
        })
        .await
        .unwrap();

    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.agent_identities.len(), 2);
    assert!(!bundle.agent_identity_events.is_empty());
    assert_eq!(bundle.agents.len(), 1);
    assert!(bundle.agents[0].identity_id.is_none());
    assert_eq!(bundle.agent_identities[0].project_id, project.id);

    let imported_data = TempDir::new().unwrap();
    let imported_store = Store::open(imported_data.path()).unwrap();
    imported_store.import_session_bundle(&bundle).unwrap();
    let imported_leader = imported_store
        .get_agent_identity(&leader.identity.id)
        .unwrap()
        .unwrap();
    assert_eq!(imported_leader.origin, "imported");
    assert_eq!(imported_leader.project_id, project.id);
    let imported_events = imported_store
        .list_agent_identity_events_for_project(&project.id, None)
        .unwrap();
    assert_eq!(imported_events.len(), bundle.agent_identity_events.len());
    let imported_engine = Engine::new(imported_store);
    let error = imported_engine
        .pause_agent_identity(PauseAgentIdentityParams {
            identity_id: leader.identity.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        typed_code(&error),
        agent_identity_codes::IDENTITY_IMPORTED_READONLY
    );
    let agents = imported_engine.store().list_agents(&session.id).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, agent.id);
    assert!(agents[0].identity_id.is_none());
    let imported_session = imported_engine
        .store()
        .get_session(&session.id)
        .unwrap()
        .unwrap();
    assert_eq!(imported_session.status, "imported");
    assert!(
        imported_engine
            .replay_agent_identities(AgentIdentityReplayParams {
                project_id: project.id,
                repair: false,
            })
            .await
            .unwrap()
            .consistent
    );
    let _ = repo;
}

#[tokio::test]
async fn update_role_changes_revalidate_parent_and_leader_uniqueness() {
    let (_data, _repo, engine, project) = harness("role-change");
    let leader = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap();
    let manager = engine
        .create_agent_identity(params(&project.id, "manager", Some(&leader.identity.id)))
        .await
        .unwrap();
    let error = engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
            role: Some("worker".to_string()),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(typed_code(&error), agent_identity_codes::IDENTITY_INVALID);
    let changed = engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
            role: Some("worker".to_string()),
            parent_identity_id: Some(None),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    assert!(changed.changed);
    assert_eq!(changed.identity.role, "worker");
    assert_eq!(changed.identity.generation, 2);
    let unchanged = engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: manager.identity.id.clone(),
            role: Some("worker".to_string()),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    assert!(!unchanged.changed);
    let error = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap_err();
    assert_eq!(typed_code(&error), agent_identity_codes::IDENTITY_CONFLICT);
}

#[tokio::test]
async fn event_payloads_are_bounded_and_malformed_replay_fails_closed() {
    let (_data, _repo, engine, project) = harness("events");
    let leader = engine
        .create_agent_identity(params(&project.id, "leader", None))
        .await
        .unwrap();
    engine
        .pause_agent_identity(PauseAgentIdentityParams {
            identity_id: leader.identity.id.clone(),
        })
        .await
        .unwrap();
    let events = engine
        .store()
        .list_agent_identity_events_for_project(&project.id, None)
        .unwrap();
    assert_eq!(events.len(), 2);
    for event in &events {
        let object = event.payload.as_object().unwrap();
        assert!(object.contains_key("identity"));
        assert!(object.contains_key("changed_fields"));
        assert!(object.contains_key("previous_generation"));
        assert!(object.contains_key("generation"));
        assert!(!object.contains_key("prompt"));
        assert!(!object.contains_key("secret"));
        let identity = object["identity"].as_object().unwrap();
        assert!(!identity.contains_key("prompt"));
        assert!(!identity.contains_key("api_key"));
        assert!(!identity.contains_key("token"));
    }

    // Corrupt one archived event payload; replay must fail closed instead of
    // inventing a projection row.
    let mut corrupt = events[0].clone();
    corrupt.payload = serde_json::json!({"not_identity": true});
    engine
        .store()
        .insert_agent_identity_event_snapshot(&corrupt)
        .unwrap();
    let error = engine
        .replay_agent_identities(AgentIdentityReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap_err();
    assert_eq!(typed_code(&error), agent_identity_codes::IDENTITY_INVALID);
}

#[tokio::test]
async fn replay_repair_is_scoped_to_one_project() {
    let (_data, _repo, engine, project_a) = harness("repair-a");
    let project_b = engine
        .store()
        .add_project(
            TempDir::new().unwrap().path().to_str().unwrap(),
            Some("repair-b"),
        )
        .unwrap();
    let leader_a = engine
        .create_agent_identity(params(&project_a.id, "leader", None))
        .await
        .unwrap();
    let leader_b = engine
        .create_agent_identity(params(&project_b.id, "leader", None))
        .await
        .unwrap();
    let mut corrupt_a = engine
        .get_agent_identity(&leader_a.identity.id)
        .await
        .unwrap()
        .identity;
    corrupt_a.generation = 77;
    engine
        .store()
        .replace_agent_identity_projection(&project_a.id, &[corrupt_a])
        .unwrap();
    let repaired = engine
        .replay_agent_identities(AgentIdentityReplayParams {
            project_id: project_a.id.clone(),
            repair: true,
        })
        .await
        .unwrap();
    assert!(repaired.repaired);
    assert!(repaired.consistent);
    let untouched = engine
        .get_agent_identity(&leader_b.identity.id)
        .await
        .unwrap()
        .identity;
    assert_eq!(untouched.generation, 1);
    assert_eq!(untouched.status, "active");
    let replay_b = engine
        .replay_agent_identities(AgentIdentityReplayParams {
            project_id: project_b.id,
            repair: false,
        })
        .await
        .unwrap();
    assert!(replay_b.consistent);
}

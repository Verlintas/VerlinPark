//! End-to-end scaling surface tests through the public engine/service APIs.

use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::AgentService;
use vp_protocol::scaling::{ScalingAction, ScalingPolicy};
use vp_protocol::types::{PlanDepartment, PlanTask, TeamPlan};
use vp_store::Store;

fn engine() -> (TempDir, TempDir, Engine, vp_protocol::types::Project) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("scaling-surface"))
        .unwrap();
    (data, repo, Engine::new(store), project)
}

fn plan(tasks: Vec<PlanTask>) -> TeamPlan {
    TeamPlan {
        goal: "scale".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks,
    }
}

fn task(id: &str, scope: &str) -> PlanTask {
    PlanTask {
        id: id.into(),
        department: "impl".into(),
        title: id.into(),
        kind: "coding".into(),
        priority: 0,
        profile: None,
        deps: Vec::new(),
        write_scope: vec![scope.into()],
        acceptance: vec!["done".into()],
        token_budget: None,
        cost_budget_usd: None,
        parent_task_id: None,
        fanout_key: None,
        fanout_join_for: None,
    }
}

async fn create_session(
    engine: &Engine,
    project_id: &str,
    goal: &str,
) -> vp_protocol::types::Session {
    let session = engine.store().create_session(project_id, goal).unwrap();
    engine
        .append_event(
            &session.id,
            "session.created",
            serde_json::json!({
                "project_id": project_id,
                "goal": goal,
                "created_at": session.created_at,
            }),
        )
        .await
        .unwrap();
    session
}

async fn idle_session_with_plan(
    engine: &Engine,
    project_id: &str,
    tasks: Vec<PlanTask>,
) -> vp_protocol::types::Session {
    let session = create_session(engine, project_id, "scale a plan").await;
    engine
        .set_team_plan(&session.id, &plan(tasks))
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn scaling_policy_round_trips_through_the_engine_surface() {
    let (_data, _repo, engine, project) = engine();
    let initial = engine.get_scaling_policy(&project.id).await.unwrap();
    assert!(!initial.policy.enabled);
    assert_eq!(initial.policy.max_workers, 4);

    let policy = ScalingPolicy {
        enabled: true,
        min_workers: 2,
        max_workers: 6,
        min_labour: 2,
        max_labour: 10,
        expand_ready_threshold: 3,
        expand_critical_delay_secs: 15,
        scale_down_idle_secs: 120,
        cooldown_secs: 30,
        max_step: 3,
        budget_headroom_ratio: Some(0.2),
    };
    let saved = engine
        .set_scaling_policy(&project.id, policy.clone())
        .await
        .unwrap();
    assert_eq!(saved.policy, policy);
    assert_eq!(
        engine.get_scaling_policy(&project.id).await.unwrap().policy,
        policy
    );

    let mut invalid = policy;
    invalid.max_workers = 0;
    let error = engine
        .set_scaling_policy(&project.id, invalid)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SCALING_POLICY_INVALID"));
}

#[tokio::test]
async fn scaling_policy_is_carried_by_the_project_manifest() {
    let (data, _repo, engine, project) = engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 7,
        max_labour: 21,
        budget_headroom_ratio: Some(0.4),
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy.clone())
        .await
        .unwrap();
    engine.record_project_manifest().await.unwrap();
    let snapshots = vp_store::read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].project.id, project.id);
    assert_eq!(snapshots[0].scaling_policy, policy);
}

#[tokio::test]
async fn disabled_policy_tick_is_a_noop_with_a_stored_plan() {
    let (_data, _repo, engine, project) = engine();
    let session = idle_session_with_plan(
        &engine,
        &project.id,
        vec![task("T1", "src/a"), task("T2", "src/b")],
    )
    .await;
    let service = AgentService::unconfigured(engine.clone());
    let result = service.scaling_tick().await.unwrap();
    assert_eq!(result.evaluated, 0);
    assert_eq!(result.persisted, 0);
    assert!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn enabled_policy_tick_persists_a_hold_when_the_ceiling_already_covers_demand() {
    let (_data, _repo, engine, project) = engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 10,
        max_labour: 40,
        expand_critical_delay_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    let tasks = (0..10)
        .map(|index| task(&format!("T{index}"), &format!("src/{index}")))
        .collect::<Vec<_>>();
    let session = idle_session_with_plan(&engine, &project.id, tasks).await;
    let service = AgentService::unconfigured(engine.clone());
    let result = service.scaling_tick().await.unwrap();
    assert_eq!(result.evaluated, 1);
    assert_eq!(result.persisted, 1);
    assert_eq!(result.holds, 1);
    assert_eq!(result.expanded, 0);
    let latest = engine
        .latest_scaling_decision(&session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.action, ScalingAction::Hold);
    assert_eq!(latest.plan_revision, 1);
    assert!(!latest.applied);
    assert!(
        latest
            .reasons
            .iter()
            .any(|reason| reason == "ready_tasks:10")
    );

    let repeat = service.scaling_tick().await.unwrap();
    assert_eq!(repeat.persisted, 0);
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.scaling_decisions_match, "{:?}", report.mismatches);
}

#[tokio::test]
async fn imported_sessions_are_not_evaluated_by_the_tick() {
    let (_data, _repo, engine, project) = engine();
    let session = idle_session_with_plan(
        &engine,
        &project.id,
        vec![task("T1", "src/a"), task("T2", "src/b")],
    )
    .await;
    engine
        .update_session_status(&session.id, "imported")
        .await
        .unwrap();
    let policy = ScalingPolicy {
        enabled: true,
        expand_critical_delay_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    let service = AgentService::unconfigured(engine.clone());
    let result = service.scaling_tick().await.unwrap();
    assert_eq!(result.evaluated, 0);
    assert!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let preview = service.scaling_preview(&session.id).await.unwrap();
    assert_eq!(preview.decision.action, ScalingAction::Hold);
    assert!(
        preview
            .decision
            .reasons
            .iter()
            .any(|reason| reason == "imported_read_only")
    );
}

#[tokio::test]
async fn preview_and_status_are_read_only_public_surfaces() {
    let (_data, _repo, engine, project) = engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 6,
        max_labour: 8,
        expand_critical_delay_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy.clone())
        .await
        .unwrap();
    let session = idle_session_with_plan(
        &engine,
        &project.id,
        vec![
            task("T1", "src/a"),
            task("T2", "src/b"),
            task("T3", "src/c"),
        ],
    )
    .await;
    let service = AgentService::unconfigured(engine.clone());

    let preview = service.scaling_preview(&session.id).await.unwrap();
    assert_eq!(preview.session_id, session.id);
    assert_eq!(preview.policy, policy);
    assert!(!preview.decision.applied);
    assert!(
        engine
            .list_scaling_decisions(&session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let status = service
        .scaling_status(Some(&project.id), Some(&session.id), Some(10))
        .await
        .unwrap();
    assert_eq!(status.projects.len(), 1);
    assert!(status.projects[0].policy.enabled);
    assert_eq!(status.projects[0].sessions.len(), 1);
    let entry = &status.projects[0].sessions[0];
    assert_eq!(entry.session_id, session.id);
    assert_eq!(entry.plan_tasks, 3);
    assert!(entry.latest.is_none());
    assert!(entry.current_workers > 0);
    assert!(entry.current_labour > 0);
}

#[tokio::test]
async fn status_project_filter_excludes_unrelated_projects() {
    let (_data, _repo, engine, project) = engine();
    let other_repo = TempDir::new().unwrap();
    let other = engine
        .store()
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    let session = idle_session_with_plan(&engine, &project.id, vec![task("T1", "src/a")]).await;
    let service = AgentService::unconfigured(engine.clone());
    let status = service
        .scaling_status(Some(&project.id), None, Some(10))
        .await
        .unwrap();
    assert_eq!(status.projects.len(), 1);
    assert_eq!(status.projects[0].project_id, project.id);
    assert_eq!(status.projects[0].sessions[0].session_id, session.id);

    let all = service.scaling_status(None, None, Some(10)).await.unwrap();
    assert_eq!(all.projects.len(), 2);
    assert!(all.projects.iter().any(|row| row.project_id == other.id));
}

#[tokio::test]
async fn scaling_tick_skips_sessions_without_a_plan() {
    let (_data, _repo, engine, project) = engine();
    let policy = ScalingPolicy {
        enabled: true,
        expand_critical_delay_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    let session = create_session(&engine, &project.id, "no plan").await;
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let service = AgentService::unconfigured(engine.clone());
    let result = service.scaling_tick().await.unwrap();
    assert_eq!(result.sessions, 0);
    assert_eq!(result.evaluated, 0);
}

#[tokio::test]
async fn preview_decision_hash_is_deterministic_for_unchanged_inputs() {
    let (_data, _repo, engine, project) = engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 6,
        max_labour: 8,
        expand_critical_delay_secs: 0,
        cooldown_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    let session = idle_session_with_plan(
        &engine,
        &project.id,
        vec![task("T1", "src/a"), task("T2", "src/b")],
    )
    .await;
    let service = AgentService::unconfigured(engine);
    let first = service.scaling_preview(&session.id).await.unwrap();
    let second = service.scaling_preview(&session.id).await.unwrap();
    assert_eq!(first.decision.inputs_hash, second.decision.inputs_hash);
    // The preview time is part of the hash, but a same-second evaluation is
    // stable; the action and ordered reasons are stable regardless.
    assert_eq!(first.decision.action, second.decision.action);
    assert_eq!(first.decision.reasons, second.decision.reasons);
}

#[tokio::test]
async fn a_tick_never_creates_managers_or_rewrites_the_plan() {
    let (_data, _repo, engine, project) = engine();
    let policy = ScalingPolicy {
        enabled: true,
        max_workers: 10,
        max_labour: 40,
        expand_critical_delay_secs: 0,
        ..ScalingPolicy::default()
    };
    engine
        .set_scaling_policy(&project.id, policy)
        .await
        .unwrap();
    let tasks = (0..8)
        .map(|index| task(&format!("T{index}"), &format!("src/{index}")))
        .collect::<Vec<_>>();
    let session = idle_session_with_plan(&engine, &project.id, tasks).await;
    let before = engine
        .get_team_plan(&session.id)
        .await
        .unwrap()
        .expect("plan");
    let before_agents = engine.list_agents(&session.id).await.unwrap();
    let service = AgentService::unconfigured(engine.clone());
    service.scaling_tick().await.unwrap();
    let after = engine
        .get_team_plan(&session.id)
        .await
        .unwrap()
        .expect("plan");
    assert_eq!(before, after);
    let after_agents = engine.list_agents(&session.id).await.unwrap();
    assert_eq!(before_agents.len(), after_agents.len());
    assert!(
        after_agents
            .iter()
            .all(|agent| !matches!(agent.role.as_str(), "manager" | "worker" | "labour"))
    );
}

#[tokio::test]
async fn a_tick_evaluates_only_enabled_projects() {
    let (_data, _repo, engine, enabled) = engine();
    let other_repo = TempDir::new().unwrap();
    let disabled = engine
        .store()
        .add_project(other_repo.path().to_str().unwrap(), Some("disabled"))
        .unwrap();
    engine
        .set_scaling_policy(
            &enabled.id,
            ScalingPolicy {
                enabled: true,
                expand_critical_delay_secs: 0,
                ..ScalingPolicy::default()
            },
        )
        .await
        .unwrap();
    engine
        .set_scaling_policy(
            &disabled.id,
            ScalingPolicy {
                enabled: false,
                ..ScalingPolicy::default()
            },
        )
        .await
        .unwrap();
    let enabled_session = idle_session_with_plan(
        &engine,
        &enabled.id,
        vec![task("T1", "src/a"), task("T2", "src/b")],
    )
    .await;
    let disabled_session = idle_session_with_plan(
        &engine,
        &disabled.id,
        vec![task("T1", "src/a"), task("T2", "src/b")],
    )
    .await;
    let service = AgentService::unconfigured(engine.clone());
    let result = service.scaling_tick().await.unwrap();
    assert_eq!(result.projects, 2);
    assert_eq!(result.evaluated, 1);
    assert_eq!(result.persisted, 1);
    assert!(
        !engine
            .list_scaling_decisions(&enabled_session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .list_scaling_decisions(&disabled_session.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

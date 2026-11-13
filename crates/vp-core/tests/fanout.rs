//! C7 bounded fan-out integration tests.
//!
//! These exercise the real Engine/Store path: plan revision write, scheduler
//! projection sync, explicit approval, artifact-sourced items, idempotent
//! replay, and the absence of any automatic expansion.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use vp_core::Engine;
use vp_core::fanout::{FanoutError, codes};
use vp_protocol::types::{PlanDepartment, PlanFanoutItem, PlanFanoutParams, PlanTask, TeamPlan};
use vp_store::Store;

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "fan-out integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "fanout@example.test"]);
    run(&["config", "user.name", "Fanout Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

fn engine() -> (TempDir, Engine, String) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("fanout"))
        .unwrap();
    (dir, engine, project.id)
}

fn task(id: &str, title: &str, deps: Vec<String>, scope: Vec<String>) -> PlanTask {
    PlanTask {
        id: id.to_string(),
        department: "impl".to_string(),
        title: title.to_string(),
        kind: "coding".to_string(),
        priority: 0,
        profile: Some("coding".to_string()),
        deps,
        write_scope: scope,
        acceptance: vec!["done".to_string()],
        token_budget: Some(1_000),
        cost_budget_usd: Some(1.0),
        parent_task_id: None,
        fanout_key: None,
        fanout_join_for: None,
    }
}

fn plan() -> TeamPlan {
    TeamPlan {
        goal: "bounded fan-out".to_string(),
        departments: vec![PlanDepartment {
            name: "impl".to_string(),
            manager_title: "Implementation".to_string(),
            profile: "coding".to_string(),
            objective: "implement components".to_string(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![
            task("T1", "Build components", vec![], vec!["src".to_string()]),
            task(
                "T2",
                "Verify",
                vec!["T1".to_string()],
                vec!["docs".to_string()],
            ),
        ],
    }
}

fn item(key: &str, scope: &str) -> PlanFanoutItem {
    PlanFanoutItem {
        key: key.to_string(),
        write_scope: vec![scope.to_string()],
        ..PlanFanoutItem::default()
    }
}

fn params(session_id: &str) -> PlanFanoutParams {
    PlanFanoutParams {
        session_id: session_id.to_string(),
        template_task_id: "T1".to_string(),
        items: vec![item("auth", "src/auth"), item("api", "src/api")],
        approved: true,
        ..PlanFanoutParams::default()
    }
}

fn code(error: &anyhow::Error) -> &'static str {
    error.downcast_ref::<FanoutError>().unwrap().code
}

#[tokio::test]
async fn explicit_fanout_writes_one_revision_and_is_idempotent() {
    let (_dir, engine, project_id) = engine();
    let session = engine
        .create_session(project_id, "fan out".to_string(), None)
        .await
        .unwrap();
    engine.set_team_plan(&session.id, &plan()).await.unwrap();

    let mut unapproved = params(&session.id);
    unapproved.approved = false;
    let error = engine.fanout_plan(unapproved).await.unwrap_err();
    assert_eq!(code(&error), codes::FANOUT_APPROVAL_REQUIRED);
    assert!(engine.get_team_plan(&session.id).await.unwrap().is_some());

    let result = engine.fanout_plan(params(&session.id)).await.unwrap();
    assert!(!result.idempotent);
    assert_eq!(result.revision, 2);
    assert_eq!(result.child_task_ids.len(), 2);
    assert_eq!(result.parent_task_id, "T1");
    assert!(!result.plan.tasks.iter().any(|task| task.id == "T1"));
    let join = result
        .plan
        .tasks
        .iter()
        .find(|task| task.id == result.join_task_id)
        .unwrap();
    assert_eq!(join.fanout_join_for.as_deref(), Some("T1"));
    assert_eq!(join.deps.len(), 2);
    let verify = result
        .plan
        .tasks
        .iter()
        .find(|task| task.id == "T2")
        .unwrap();
    assert_eq!(verify.deps, vec![result.join_task_id.clone()]);
    assert!(result.plan.tasks.iter().any(|task| {
        task.fanout_key.as_deref() == Some("auth")
            && task.parent_task_id.as_deref() == Some("T1")
            && task.token_budget == Some(1_000)
            && task.cost_budget_usd == Some(1.0)
    }));

    let revisions = engine.plan_revisions(&session.id).await.unwrap();
    assert_eq!(revisions.len(), 2);
    let revised = engine
        .recent_events_of_kind(&session.id, "team_plan.revised", 1)
        .await
        .unwrap();
    assert_eq!(revised.len(), 1);
    assert_eq!(revised[0].payload["fanout"]["parent_task_id"], "T1");
    assert_eq!(
        revised[0].payload["fanout"]["join_task_id"],
        result.join_task_id
    );
    assert_eq!(revised[0].payload["fanout"]["item_source"], "inline");

    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap();
    assert!(
        result
            .child_task_ids
            .iter()
            .all(|id| queue.tasks.iter().any(|task| &task.task_id == id))
    );

    let replay = engine.fanout_plan(params(&session.id)).await.unwrap();
    assert!(replay.idempotent);
    assert_eq!(replay.revision, 2);
    assert_eq!(
        engine.plan_revisions(&session.id).await.unwrap().len(),
        2,
        "an idempotent replay must not append a revision"
    );

    let mut conflicting = params(&session.id);
    conflicting.items = vec![item("different", "src/other")];
    let error = engine.fanout_plan(conflicting).await.unwrap_err();
    assert_eq!(code(&error), codes::FANOUT_CONFLICT);

    let mut stale = params(&session.id);
    stale.expected_revision = Some(1);
    let error = engine.fanout_plan(stale).await.unwrap_err();
    assert_eq!(code(&error), codes::FANOUT_CONFLICT);
}

#[tokio::test]
async fn artifact_items_are_bounded_and_read_from_the_session() {
    let (_dir, engine, project_id) = engine();
    let session = engine
        .create_session(project_id, "artifact fan out".to_string(), None)
        .await
        .unwrap();
    engine.set_team_plan(&session.id, &plan()).await.unwrap();
    let items =
        serde_json::to_string(&vec![item("auth", "src/auth"), item("api", "src/api")]).unwrap();
    let artifact = engine
        .publish_artifact(
            &session.id,
            "leader",
            "Leader",
            None,
            "fanout-items",
            "plan_fanout_items",
            "application/json",
            &items,
        )
        .await
        .unwrap();
    let mut artifact_params = params(&session.id);
    artifact_params.items.clear();
    artifact_params.artifact_id = Some(artifact.id.clone());
    let result = engine.fanout_plan(artifact_params).await.unwrap();
    assert_eq!(result.child_task_ids.len(), 2);
    let revised = engine
        .recent_events_of_kind(&session.id, "team_plan.revised", 1)
        .await
        .unwrap();
    assert_eq!(revised[0].payload["fanout"]["item_source"], "artifact");
    assert_eq!(revised[0].payload["fanout"]["artifact_id"], artifact.id);

    let mut both = params(&session.id);
    both.artifact_id = Some(artifact.id);
    let error = engine.fanout_plan(both).await.unwrap_err();
    assert_eq!(code(&error), codes::FANOUT_INVALID);
}

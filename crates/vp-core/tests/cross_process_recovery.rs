//! Public-API recovery tests for cross-process ownership.
//!
//! These complement the store-level tests: they exercise `Engine` recovery and
//! the operational rows an operator sees after a second daemon starts.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use vp_core::Engine;
use vp_store::Store;

fn init_repo(path: &Path) {
    std::fs::write(path.join("README.md"), "hello\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@verlinpark.local"],
        vec!["config", "user.name", "VerlinPark Test"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

async fn engine_and_session(label: &str) -> (TempDir, TempDir, Engine, String, String) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some(label.into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), label.into(), None)
        .await
        .unwrap();
    (data, repo, engine, project.id, session.id)
}

#[tokio::test]
async fn live_foreign_session_lease_preserves_pending_work_until_release() {
    let (data, _repo, engine, project_id, session_id) = engine_and_session("foreign work").await;
    engine
        .update_session_status(&session_id, "running")
        .await
        .unwrap();
    let store = engine.store().clone();
    let foreign = store
        .acquire_runtime_lease(
            &format!("session:{session_id}"),
            "foreign-owner",
            "foreign-instance:999",
            "session",
            Some(&session_id),
            60,
        )
        .unwrap();
    let approval = store
        .create_approval(
            &session_id,
            "call-1",
            "write_file",
            &serde_json::json!({"path": "README.md"}),
            Some("workspace"),
            Some("write"),
        )
        .unwrap();
    let question = store
        .create_question_with_event(
            &session_id,
            "agent-1",
            "Agent",
            "Continue?",
            &["yes".into(), "no".into()],
        )
        .unwrap()
        .0;
    let steering = store
        .create_steering_with_event(&session_id, "agent-1", None, "keep going")
        .unwrap()
        .0;

    let restarted = Engine::new(Store::open(data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let session = restarted.store().get_session(&session_id).unwrap().unwrap();
    assert_eq!(session.status, "running");
    assert_eq!(
        restarted
            .list_approvals(Some(session_id.clone()))
            .await
            .unwrap()
            .iter()
            .find(|item| item.id == approval.id)
            .unwrap()
            .status,
        "pending"
    );
    assert_eq!(
        restarted
            .store()
            .list_questions(Some(&session_id))
            .unwrap()
            .iter()
            .find(|item| item.id == question.id)
            .unwrap()
            .status,
        "pending"
    );
    assert_eq!(
        restarted
            .store()
            .list_steering(&session_id, None, None)
            .unwrap()
            .iter()
            .find(|item| item.id == steering.id)
            .unwrap()
            .status,
        "pending"
    );
    assert!(
        restarted
            .list_events(session_id.clone(), 0, 100)
            .await
            .unwrap()
            .iter()
            .all(|event| event.kind != "agent.failed")
    );

    store
        .release_runtime_lease(
            &format!("session:{session_id}"),
            "foreign-owner",
            foreign.token(),
        )
        .unwrap();
    restarted.recover_interrupted_sessions().await.unwrap();
    let session = restarted.store().get_session(&session_id).unwrap().unwrap();
    assert_eq!(session.status, "failed");
    assert_eq!(
        restarted
            .list_approvals(Some(session_id.clone()))
            .await
            .unwrap()
            .iter()
            .find(|item| item.id == approval.id)
            .unwrap()
            .status,
        "denied"
    );
    assert_eq!(
        restarted
            .store()
            .list_questions(Some(&session_id))
            .unwrap()
            .iter()
            .find(|item| item.id == question.id)
            .unwrap()
            .status,
        "expired"
    );
    assert_eq!(
        restarted
            .store()
            .list_steering(&session_id, None, None)
            .unwrap()
            .iter()
            .find(|item| item.id == steering.id)
            .unwrap()
            .status,
        "cancelled"
    );
    let _ = project_id;
}

#[tokio::test]
async fn expired_session_lease_is_recoverable_by_the_next_daemon() {
    let (data, _repo, engine, _project_id, session_id) = engine_and_session("expired lease").await;
    engine
        .update_session_status(&session_id, "running")
        .await
        .unwrap();
    engine
        .store()
        .acquire_runtime_lease(
            &format!("session:{session_id}"),
            "previous-daemon",
            "previous-instance:111",
            "session",
            Some(&session_id),
            0,
        )
        .unwrap();
    let restarted = Engine::new(Store::open(data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let session = restarted.store().get_session(&session_id).unwrap().unwrap();
    assert_eq!(session.status, "failed");
    let events = restarted
        .list_events(session_id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.updated" && event.payload["status"] == "failed")
    );
}

#[tokio::test]
async fn engine_lease_audit_sees_acquire_release_and_startup_cleanup() {
    let (data, _repo, engine, _project_id, session_id) = engine_and_session("audit").await;
    let lease = engine
        .acquire_runtime_lease(
            "integration",
            &session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let generation = lease.generation();
    lease.release().await;
    let entries = engine
        .runtime_lease_audit(Some(&format!("integration:{session_id}")), Some(20))
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].action, "released");
    assert_eq!(entries[0].fencing_token, generation);
    assert_eq!(entries[1].action, "acquired");
    assert!(entries.iter().all(|entry| {
        !serde_json::to_string(entry)
            .unwrap()
            .contains("lease_token")
    }));

    // A lease held by this process across recovery is cleaned and audited.
    engine
        .acquire_runtime_lease(
            "integration",
            &session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let restarted = Engine::new(Store::open(data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let entries = restarted
        .runtime_lease_audit(Some(&format!("integration:{session_id}")), Some(20))
        .await
        .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "startup_cleanup"),
        "{entries:?}"
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.fencing_token <= generation + 1)
    );
}

#[tokio::test]
async fn audit_prune_does_not_touch_live_leases() {
    let (data, _repo, engine, _project_id, session_id) = engine_and_session("audit prune").await;
    let lease = engine
        .acquire_runtime_lease(
            "integration",
            &session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let audit = engine.runtime_lease_audit(None, Some(10)).await.unwrap();
    assert_eq!(audit.len(), 1);
    // A zero-day retention is clamped to one day, so today's rows survive.
    let removed = engine.prune_runtime_lease_audit(0).await.unwrap();
    assert_eq!(removed, 0);
    assert_eq!(engine.list_runtime_leases().await.unwrap().len(), 1);
    lease.release().await;
    assert!(engine.list_runtime_leases().await.unwrap().is_empty());
    let _ = data;
}

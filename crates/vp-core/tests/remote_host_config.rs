//! Public integration tests for project execution-host configuration.
//!
//! These tests use the same `Store`/`Engine` API the daemon uses. They verify
//! that local remains the default, remote configuration is validated, the
//! project manifest captures it, and tools never get a remote access bundle
//! for a local project.

use tempfile::TempDir;
use vp_core::Engine;
use vp_protocol::remote::{ExecutionHost, ExecutionHostMode};
use vp_store::Store;

fn engine() -> (TempDir, TempDir, Engine, String) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("remote-test"))
        .unwrap();
    (data, repo, Engine::new(store), project.id)
}

fn remote_config() -> ExecutionHost {
    ExecutionHost {
        mode: ExecutionHostMode::Ssh,
        host: "build.example.com".into(),
        user: Some("agent".into()),
        root: "/srv/project".into(),
        token_env: Some("VERLINPARK_REMOTE_TOKEN_INTEGRATION".into()),
        ..ExecutionHost::default()
    }
}

#[tokio::test]
async fn local_is_the_default_and_has_no_remote_access() {
    let (_data, _repo, engine, project_id) = engine();
    assert_eq!(
        engine.get_execution_host(&project_id).await.unwrap(),
        ExecutionHost::default()
    );
    assert!(engine.remote_access(&project_id).await.unwrap().is_none());
    let status = engine.remote_status(&project_id).await.unwrap();
    assert_eq!(status.mode, ExecutionHostMode::Local);
    assert!(!status.configured);
    assert!(
        engine
            .probe_execution_host(&project_id, Some(1))
            .await
            .unwrap_err()
            .to_string()
            .contains("local")
    );
}

#[tokio::test]
async fn remote_host_lifecycle_is_validated_and_manifested() {
    let (data, _repo, engine, project_id) = engine();
    let config = remote_config();
    engine
        .set_execution_host(&project_id, config.clone())
        .await
        .unwrap();
    assert_eq!(
        engine.get_execution_host(&project_id).await.unwrap(),
        config
    );
    let hosts = engine.list_execution_hosts().await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].project_id, project_id);
    assert_eq!(hosts[0].config, config);
    let access = engine.remote_access(&project_id).await.unwrap().unwrap();
    assert_eq!(access.project_id, project_id);
    let status = engine.remote_status(&project_id).await.unwrap();
    assert_eq!(status.mode, ExecutionHostMode::Ssh);
    assert!(status.configured);
    assert!(!status.connected);
    assert_eq!(status.root, "/srv/project");

    engine.record_project_manifest().await.unwrap();
    let manifest = vp_store::read_project_manifest(data.path()).unwrap();
    assert_eq!(manifest.len(), 1);
    assert_eq!(manifest[0].execution_host, config);

    assert!(engine.remove_execution_host(&project_id).await.unwrap());
    assert!(engine.remote_access(&project_id).await.unwrap().is_none());
    assert_eq!(
        engine.get_execution_host(&project_id).await.unwrap(),
        ExecutionHost::default()
    );
}

#[tokio::test]
async fn invalid_remote_configuration_is_refused_before_storage() {
    let (_data, _repo, engine, project_id) = engine();
    let mut config = remote_config();
    config.root = "relative".into();
    let error = engine
        .set_execution_host(&project_id, config)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("absolute"));

    let mut config = remote_config();
    config.token_env = None;
    let error = engine
        .set_execution_host(&project_id, config)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("token_env"));

    let mut config = remote_config();
    config.host = "host;id".into();
    let error = engine
        .set_execution_host(&project_id, config)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("safely"));
}

#[tokio::test]
async fn remote_tool_risk_is_inspectable_through_the_public_api() {
    use vp_core::permissions::{RiskLevel, assess_tool_call};

    let worktree = TempDir::new().unwrap();
    let read = assess_tool_call(
        "remote_read",
        &serde_json::json!({"path": "a"}),
        worktree.path(),
    );
    assert_eq!(read.risk, RiskLevel::ReadOnly);
    let write = assess_tool_call(
        "remote_write",
        &serde_json::json!({"path": "a"}),
        worktree.path(),
    );
    assert_eq!(write.risk, RiskLevel::External);
    let exec = assess_tool_call(
        "remote_exec",
        &serde_json::json!({"command": "make"}),
        worktree.path(),
    );
    assert_eq!(exec.risk, RiskLevel::HostState);
}

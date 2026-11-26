use super::protocol_transport_tests::empty_state;
use super::*;

use serde_json::json;
use tempfile::TempDir;
use vp_protocol::rpc::RpcRequest;

#[tokio::test]
async fn project_add_rejects_a_non_git_directory_before_insert() {
    let (_data, state) = empty_state();
    let root = TempDir::new().unwrap();
    let response = dispatch(
        &state,
        RpcRequest::new(json!(1), "project.add", json!({"root": root.path()})),
    )
    .await;
    let error = response.error.expect("non-Git project must fail");
    assert!(
        error.message.contains("Git repository"),
        "unexpected error: {}",
        error.message
    );
    assert!(state.engine.list_projects().await.unwrap().is_empty());
}

#[tokio::test]
async fn project_remove_requires_explicit_confirmation() {
    let (_data, state) = empty_state();
    let response = dispatch(
        &state,
        RpcRequest::new(json!(1), "project.remove", json!({"project_id": "p"})),
    )
    .await;
    let error = response.error.expect("unconfirmed removal must fail");
    assert_eq!(error.code, vp_protocol::rpc::codes::INVALID_PARAMS);
    assert_eq!(
        error.data.unwrap()["code"],
        "PROJECT_REMOVE_CONFIRM_REQUIRED"
    );
}

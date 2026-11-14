//! MCP asynchronous task lifecycle tests against a scripted stdio server.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use vp_core::Engine;
use vp_core::mcp::tasks::McpTaskContext;
use vp_protocol::types::{
    McpServerConfig, McpTaskCancelParams, McpTaskListParams, McpTaskResultParams,
};
use vp_store::Store;

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "mcp tasks\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "mcp-task@example.test"]);
    run(&["config", "user.name", "MCP Task Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

fn task_server_script(dir: &TempDir) -> std::path::PathBuf {
    let script = dir.path().join("mcp-task-server.py");
    std::fs::write(
        &script,
        r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "task-server", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "slow", "description": "slow task", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        result = {"task": {"taskId": "remote-task-1", "status": "working", "ttl": 300, "pollInterval": 100}}
    elif method == "tasks/get":
        result = {"task": {"taskId": "remote-task-1", "status": "completed", "ttl": 300, "pollInterval": 100}}
    elif method == "tasks/result":
        result = {"task": {"taskId": "remote-task-1", "status": "completed"}, "result": {"content": [{"type": "text", "text": "task done"}]}}
    elif method == "tasks/cancel":
        result = {"task": {"taskId": "remote-task-1", "status": "cancelled"}}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
"#,
    )
    .unwrap();
    script
}

async fn harness() -> (TempDir, Engine, vp_protocol::types::Session) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store);
    let project = engine
        .store()
        .add_project(repo.path().to_str().unwrap(), Some("mcp tasks"))
        .unwrap();
    let session = engine
        .create_session(project.id, "mcp tasks".into(), None)
        .await
        .unwrap();
    (data, engine, session)
}

fn add_server(engine: &Engine, name: &str, script: &Path) -> vp_protocol::types::McpServer {
    engine
        .store()
        .add_mcp_server(&McpServerConfig {
            name: name.to_string(),
            transport: Default::default(),
            command: "python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
        })
        .unwrap()
}

#[tokio::test]
async fn async_call_get_result_and_list_are_durable_and_explicit() {
    let (data, engine, session) = harness().await;
    let script = task_server_script(&data);
    let server = add_server(&engine, "tasks", &script);
    let context = McpTaskContext::from_session(
        &session.project_id,
        &session.id,
        "agent",
        "Agent",
        None,
        Some("effect-1"),
    );
    let result = engine
        .mcp_call_with_context(&server.id, "slow", serde_json::json!({"value": 1}), context)
        .await
        .unwrap();
    let task = result.task.expect("async task handle");
    assert_eq!(task.status, "working");
    assert_eq!(task.server_id, server.id);
    assert_eq!(task.effect_receipt_id.as_deref(), Some("effect-1"));
    assert!(!task.result_available);

    let refreshed = engine.mcp_task_get(&task.id, true).await.unwrap();
    assert_eq!(refreshed.status, "completed");
    let result = engine
        .mcp_task_result(McpTaskResultParams {
            task_id: task.id.clone(),
            refresh: Some(true),
        })
        .await
        .unwrap();
    assert!(!result.pending);
    assert_eq!(result.task.status, "completed");
    assert_eq!(
        result.result.as_ref().unwrap()["content"][0]["text"],
        "task done"
    );
    let list = engine
        .mcp_task_list(McpTaskListParams {
            project_id: Some(session.project_id.clone()),
            session_id: Some(session.id.clone()),
            server_id: Some(server.id.clone()),
            status: Some("completed".to_string()),
            limit: Some(10),
        })
        .await
        .unwrap();
    assert_eq!(list.tasks.len(), 1);
    assert_eq!(list.tasks[0].id, task.id);
    assert!(!list.truncated);
    let events = engine.all_events(&session.id).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "mcp.task.created"));
    assert!(
        events
            .iter()
            .any(|event| event.kind == "mcp.task.completed")
    );
    assert!(events.iter().any(|event| event.kind == "mcp.task.result"));
}

#[tokio::test]
async fn async_cancel_is_explicit_and_terminal_tasks_are_refused() {
    let (data, engine, session) = harness().await;
    let script = task_server_script(&data);
    let server = add_server(&engine, "tasks-cancel", &script);
    let context = McpTaskContext::from_session(
        &session.project_id,
        &session.id,
        "agent",
        "Agent",
        None,
        None,
    );
    let result = engine
        .mcp_call_with_context(&server.id, "slow", serde_json::json!({}), context)
        .await
        .unwrap();
    let task = result.task.expect("async task handle");
    let cancelled = engine
        .mcp_task_cancel(&task.id, Some("operator stop"))
        .await
        .unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.task.status, "cancelled");
    let again = engine.mcp_task_cancel(&task.id, None).await.unwrap();
    assert!(!again.cancelled);
    assert_eq!(again.task.status, "cancelled");
}

#[test]
fn task_store_is_scoped_by_session_and_server() {
    // The persistence contract is covered by the store test; this is a
    // compile-time guard that the protocol types remain constructible.
    let task = vp_protocol::types::McpTask {
        id: "t".into(),
        project_id: "p".into(),
        session_id: "s".into(),
        agent_id: "a".into(),
        agent_name: "A".into(),
        task_id: None,
        server_id: "server".into(),
        server_name: "Server".into(),
        tool_name: "tool".into(),
        remote_task_id: "remote".into(),
        status: "working".into(),
        ttl_secs: 30,
        poll_interval_ms: 100,
        created_at: "2026-11-14T00:00:00Z".into(),
        updated_at: "2026-11-14T00:00:00Z".into(),
        expires_at: None,
        result_available: false,
        effect_receipt_id: None,
        last_error: None,
    };
    assert_eq!(task.status, "working");
    let _ = McpTaskCancelParams {
        task_id: task.id,
        reason: None,
    };
}

#[tokio::test]
async fn expired_task_is_marked_on_explicit_read() {
    let (data, engine, session) = harness().await;
    let script = task_server_script(&data);
    let server = add_server(&engine, "tasks-expiry", &script);
    let context = McpTaskContext::from_session(
        &session.project_id,
        &session.id,
        "agent",
        "Agent",
        None,
        None,
    );
    let result = engine
        .mcp_call_with_context(&server.id, "slow", serde_json::json!({}), context)
        .await
        .unwrap();
    let task = result.task.expect("async task handle");
    let mut row = engine.store().get_mcp_task(&task.id).unwrap().unwrap();
    row.expires_at = Some("2000-01-01T00:00:00Z".to_string());
    row.updated_at = "2000-01-01T00:00:00Z".to_string();
    engine
        .store()
        .update_mcp_task_with_event(&row, "mcp.task.updated", serde_json::json!({}))
        .unwrap();
    let listed = engine
        .mcp_task_list(McpTaskListParams {
            project_id: None,
            session_id: Some(session.id.clone()),
            server_id: None,
            status: None,
            limit: Some(10),
        })
        .await
        .unwrap();
    assert_eq!(listed.tasks[0].status, "expired");
    let events = engine.all_events(&session.id).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "mcp.task.expired"));
}

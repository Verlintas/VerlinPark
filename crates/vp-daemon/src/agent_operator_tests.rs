//! Agent Inspector / direct-message RPC tests.

use super::*;
use std::path::Path;
use tempfile::TempDir;
use vp_protocol::rpc::RpcRequest;
use vp_protocol::types::CreateAgentIdentityParams;

fn app_state(data: &Path, engine: Engine) -> AppState {
    AppState {
        agent: AgentService::from_env(engine.clone()),
        engine,
        token: Arc::new("test-token".to_string()),
        data_dir: data.to_path_buf(),
        started_at: now_rfc3339(),
        started: Instant::now(),
        pid: std::process::id(),
    }
}

async fn call(state: &AppState, method: &str, params: Value) -> vp_protocol::rpc::RpcResponse {
    dispatch(state, RpcRequest::new(json!(1), method, params)).await
}

#[tokio::test]
async fn agent_inspect_and_direct_message_route_with_truthful_typed_errors() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("operator-rpc"))
        .unwrap();
    let session = store.create_session(&project.id, "direct channel").unwrap();
    let engine = Engine::new(store.clone());
    let state = app_state(data.path(), engine.clone());

    let leader = engine
        .create_leader_agent(&session.id, "leader-model")
        .await
        .unwrap();
    let worker_identity = engine
        .create_agent_identity(CreateAgentIdentityParams {
            project_id: project.id.clone(),
            role: "worker".into(),
            department: Some("impl".into()),
            title: Some("Implementation".into()),
            profile: Some("coding".into()),
            ..CreateAgentIdentityParams::default()
        })
        .await
        .unwrap()
        .identity;
    let worker = engine
        .create_agent(
            &session.id,
            "worker",
            "Worker",
            Some("Implementation".into()),
            Some(leader.clone()),
            "worker-model",
        )
        .await
        .unwrap();
    engine
        .set_agent_identity_binding(&worker.id, Some(&worker_identity.id))
        .await
        .unwrap();

    // Legacy roster remains available and unchanged.
    let list = call(&state, "agent.list", json!({"session_id": session.id})).await;
    assert!(list.error.is_none());
    assert_eq!(
        list.result.unwrap()["agents"].as_array().map(Vec::len),
        Some(2)
    );

    let inspect = call(
        &state,
        "agent.inspect",
        json!({"session_id": session.id, "agent_id": leader, "timeline_limit": 20}),
    )
    .await;
    assert!(inspect.error.is_none(), "{:?}", inspect.error);
    let inspected = inspect.result.unwrap();
    assert_eq!(inspected["agent"]["id"], leader);
    assert!(inspected["timeline"].is_array());
    assert!(inspected["context"]["generated_at"].is_string());

    let other = store
        .create_session(&project.id, "another session")
        .unwrap();
    let cross = call(
        &state,
        "agent.inspect",
        json!({"session_id": other.id, "agent_id": leader}),
    )
    .await;
    let error = cross.error.expect("cross-session inspect must fail");
    assert_eq!(error.code, codes::NOT_FOUND);
    assert_eq!(
        error.data.expect("typed inspect data")["code"],
        "AGENT_NOT_FOUND"
    );

    // Idle ephemeral run: typed failure, no inbox row, no steering row.
    engine.update_agent_status(&leader, "idle").await.unwrap();
    let ephemeral = call(
        &state,
        "agent.message.send",
        json!({
            "session_id": session.id,
            "agent_id": leader,
            "body": "hello idle leader",
        }),
    )
    .await;
    let error = ephemeral.error.expect("ephemeral idle target must fail");
    assert_eq!(error.code, codes::CONFLICT);
    let typed = error.data.expect("typed not-active data");
    assert_eq!(typed["code"], "AGENT_NOT_ACTIVE");
    assert!(typed["hint"].as_str().unwrap_or_default().contains("task"));
    let inbox = engine
        .list_agent_inbox(vp_protocol::types::AgentInboxListParams {
            project_id: Some(project.id.clone()),
            session_id: Some(session.id.clone()),
            ..vp_protocol::types::AgentInboxListParams::default()
        })
        .await
        .unwrap();
    assert!(inbox.messages.is_empty());

    // Idle identity run: durable unread inbox row, no instant-delivery claim.
    engine
        .update_agent_status(&worker.id, "idle")
        .await
        .unwrap();
    let inbox = call(
        &state,
        "agent.message.send",
        json!({
            "session_id": session.id,
            "agent_id": worker.id,
            "body": "pick this up when you return",
        }),
    )
    .await;
    assert!(inbox.error.is_none(), "{:?}", inbox.error);
    let delivery = inbox.result.unwrap();
    assert_eq!(delivery["delivery"]["kind"], "inbox");
    assert_eq!(delivery["delivery"]["status"], "unread");
    assert_eq!(delivery["delivery"]["identity_id"], worker_identity.id);
    assert_eq!(
        delivery["delivery"]["inbox"]["body"],
        "pick this up when you return"
    );
    let persisted = engine
        .list_agent_inbox(vp_protocol::types::AgentInboxListParams {
            project_id: Some(project.id.clone()),
            identity_id: Some(worker_identity.id.clone()),
            ..vp_protocol::types::AgentInboxListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(persisted.messages.len(), 1);

    // A running Agent uses steering, and the same operation id replays the
    // first result instead of creating a duplicate steering row. The route
    // requires a live durable run, not only the agent status projection.
    engine
        .update_agent_status(&leader, "running")
        .await
        .unwrap();
    let run_id = "operator-rpc-run".to_string();
    let now = now_rfc3339();
    store
        .start_session_run(&vp_protocol::types::SessionRun {
            run_id: run_id.clone(),
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            agent_id: leader.clone(),
            agent_name: "Leader".into(),
            agent_role: "leader".into(),
            task_id: None,
            department: None,
            objective: "operator RPC".into(),
            provider: "test".into(),
            model: "leader-model".into(),
            status: "running".into(),
            attempt: 1,
            resumed_from_run_id: None,
            resume_root_id: run_id.clone(),
            top_level: true,
            started_at: now.clone(),
            updated_at: now,
            ended_at: None,
            budget: Default::default(),
            worktree_head: String::new(),
            plan_revision: 0,
            runtime_snapshot: None,
            last_event_seq: 0,
            failure: None,
        })
        .unwrap();
    let task_mismatch = call(
        &state,
        "agent.message.send",
        json!({
            "session_id": session.id,
            "agent_id": leader,
            "body": "wrong task",
            "task_id": "T9",
        }),
    )
    .await;
    let error = task_mismatch.error.expect("task mismatch must fail");
    assert_eq!(error.code, codes::INVALID_PARAMS);
    assert_eq!(
        error.data.expect("typed task data")["code"],
        "AGENT_TASK_INVALID"
    );

    let params = json!({
        "session_id": session.id,
        "agent_id": leader,
        "body": "check the parser",
        "operation_id": "agent-msg-op-1",
    });
    let first = call(&state, "agent.message.send", params.clone()).await;
    assert!(first.error.is_none(), "{:?}", first.error);
    let first = first.result.unwrap();
    assert_eq!(first["delivery"]["kind"], "steering");
    assert_eq!(first["delivery"]["status"], "pending");
    let steering_id = first["delivery"]["steering"]["id"]
        .as_str()
        .expect("steering id")
        .to_string();
    let replay = call(&state, "agent.message.send", params).await;
    assert!(replay.error.is_none(), "{:?}", replay.error);
    let replay = replay.result.unwrap();
    assert_eq!(
        replay["delivery"]["steering"]["id"].as_str(),
        Some(steering_id.as_str())
    );
    let steering = engine
        .session_steering(&session.id, None, None)
        .await
        .unwrap();
    assert_eq!(steering.steering.len(), 1);
    assert_eq!(steering.steering[0].id, steering_id);

    // A stale `running` row after the durable run ended must not accept a
    // second undeliverable steering message.
    store
        .finish_session_run(&run_id, "completed", None)
        .unwrap();
    let after_run = call(
        &state,
        "agent.message.send",
        json!({
            "session_id": session.id,
            "agent_id": leader,
            "body": "this run already ended",
        }),
    )
    .await;
    let error = after_run
        .error
        .expect("completed run must not accept steering");
    assert_eq!(error.code, codes::CONFLICT);
    assert_eq!(
        error.data.expect("typed not-active data")["code"],
        "AGENT_NOT_ACTIVE"
    );
    assert_eq!(
        engine
            .session_steering(&session.id, None, None)
            .await
            .unwrap()
            .steering
            .len(),
        1,
        "the completed run must not add a pending steering row"
    );
}

#[tokio::test]
async fn session_resource_readiness_and_contract_rpcs_are_read_only() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("resources-rpc"))
        .unwrap();
    let session = store.create_session(&project.id, "resource RPCs").unwrap();
    let engine = Engine::new(store);
    let state = app_state(data.path(), engine.clone());
    engine
        .create_leader_agent(&session.id, "leader-model")
        .await
        .unwrap();
    let before = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap()
        .len();

    for (method, key) in [
        ("session.resources", "dimensions"),
        ("session.readiness", "level"),
        ("session.run_contract.get", "profile"),
    ] {
        let response = call(&state, method, json!({"session_id": session.id})).await;
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        let result = response.result.unwrap();
        assert!(
            result.get(key).is_some(),
            "{method}: missing {key}: {result}"
        );
    }
    let after = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap()
        .len();
    assert_eq!(before, after, "read-only RPCs must not append events");
}

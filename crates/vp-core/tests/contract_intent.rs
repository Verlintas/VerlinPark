//! End-to-end task-contract and MCP pin persistence/replay tests.

use std::collections::BTreeMap;

use tempfile::TempDir;
use vp_core::Engine;
use vp_core::contract::{ContractCallSpec, ContractDerivation, MutationDelta, codes};
use vp_protocol::types::{
    ContractMode, ContractPolicy, McpPinApproveParams, McpPinRejectParams, McpServerConfig,
    McpTool, McpTransport, Project, Session,
};
use vp_store::Store;

fn engine() -> (TempDir, TempDir, Engine, Project) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("contracts"))
        .unwrap();
    (data, repo, Engine::new(store), project)
}

async fn session(engine: &Engine, project_id: &str) -> Session {
    let session = engine
        .store()
        .create_session(project_id, "contract test")
        .unwrap();
    engine
        .append_event(
            &session.id,
            "session.created",
            serde_json::json!({
                "project_id": project_id,
                "goal": "contract test",
                "created_at": session.created_at,
            }),
        )
        .await
        .unwrap();
    session
}

fn derivation(session: &Session, project_id: &str) -> ContractDerivation {
    ContractDerivation {
        project_id: project_id.to_string(),
        session_id: session.id.clone(),
        task_id: Some("t1".into()),
        plan_revision: 2,
        role: "worker".into(),
        profile: "coding".into(),
        goal: "contract test".into(),
        task_kind: Some("coding".into()),
        write_scope: vec!["crates/vp-core".into()],
        acceptance: vec!["tests pass".into()],
        allowed_tools: vec!["read_file".into(), "write_file".into(), "shell".into()],
        policy: ContractPolicy::default(),
        shell_network: "allow".into(),
    }
}

#[tokio::test]
async fn derived_contract_persists_and_replays() {
    let (_data, repo, engine, project) = engine();
    let session = session(&engine, &project.id).await;
    let contract = engine
        .derive_task_contract(derivation(&session, &project.id))
        .await
        .unwrap();
    assert!(contract.verify_hash());

    let latest = engine
        .latest_task_contract(&session.id, Some("t1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.contract_id, contract.contract_id);
    assert_eq!(latest.hash, contract.hash);
    assert!(!latest.created_at.is_empty());

    let worktree = repo.path();
    let input = serde_json::json!({"path": "docs/x", "content": "x"});
    let spec = ContractCallSpec::new(
        "write_file",
        &input,
        vp_core::permissions::RiskLevel::WorkspaceWrite,
        worktree,
    );
    let decision = vp_core::contract::authorize(&spec, &contract, &ContractPolicy::default());
    assert_eq!(decision.code, codes::CONTRACT_PATH_VIOLATION);
    engine
        .record_intent_decision(&contract, &decision, &spec)
        .await
        .unwrap();
    let usage = engine
        .record_intent_mutation(
            &contract,
            MutationDelta {
                writes: 1,
                bytes: 1,
                commits: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(usage.writes, 1);

    let status = engine
        .session_contract_status(&session.id, Some("t1"))
        .await
        .unwrap();
    assert_eq!(status.contract.unwrap().writes_used, 1);
    assert_eq!(status.recent_decisions.len(), 1);
    assert_eq!(status.recent_decisions[0].decision, "escalate");

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.contracts_match, "{report:?}");
    assert!(report.consistent, "{report:?}");

    let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
    assert!(rebuilt.rebuilt);
    assert!(rebuilt.contracts_match, "{rebuilt:?}");
}

#[tokio::test]
async fn contract_policy_round_trips_and_normalizes() {
    let (_data, _repo, engine, project) = engine();
    let default = engine.get_contract_policy(&project.id).await.unwrap();
    assert!(default.enabled);
    assert_eq!(default.mode, ContractMode::Enforce);

    let policy = ContractPolicy {
        mode: ContractMode::Advisory,
        enforce_shell: true,
        egress_allowlist: vec!["example.com".into()],
        max_writes: Some(4),
        ..ContractPolicy::default()
    };
    let stored = engine
        .set_contract_policy(&project.id, policy)
        .await
        .unwrap();
    assert_eq!(stored.mode, ContractMode::Advisory);
    let read = engine.get_contract_policy(&project.id).await.unwrap();
    assert_eq!(read, stored);

    let invalid = ContractPolicy {
        forbidden_paths: vec!["/etc".into()],
        ..ContractPolicy::default()
    };
    assert!(
        engine
            .set_contract_policy(&project.id, invalid)
            .await
            .is_err()
    );
}

fn server_config() -> McpServerConfig {
    McpServerConfig {
        name: "demo".into(),
        transport: McpTransport::Stdio,
        command: "demo-mcp".into(),
        args: vec!["--stdio".into()],
        env: BTreeMap::from([("DEMO_TOKEN".into(), "${DEMO_TOKEN}".into())]),
        url: None,
        headers: BTreeMap::new(),
        enabled: true,
    }
}

fn tool(description: &str, schema: serde_json::Value) -> McpTool {
    McpTool {
        name: "echo".into(),
        description: description.into(),
        input_schema: schema,
    }
}

#[tokio::test]
async fn mcp_pins_tofu_drift_approve_and_reject() {
    let (_data, _repo, engine, project) = engine();
    let session = session(&engine, &project.id).await;
    let server = engine.store().add_mcp_server(&server_config()).unwrap();

    let pins = engine
        .sync_mcp_pins(
            &session.id,
            &project.id,
            &server,
            &[tool("Echo", serde_json::json!({"type": "object"}))],
        )
        .await
        .unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].status, "pinned");
    assert_eq!(pins[0].approved_by.as_deref(), Some("tofu"));

    // Unchanged discovery does not create drift or a new approval prompt.
    let pins = engine
        .sync_mcp_pins(
            &session.id,
            &project.id,
            &server,
            &[tool("Echo", serde_json::json!({"type": "object"}))],
        )
        .await
        .unwrap();
    assert_eq!(pins[0].status, "pinned");

    // A changed schema moves to drift and keeps the approved hash.
    let approved_schema = pins[0].schema_hash.clone();
    let pins = engine
        .sync_mcp_pins(
            &session.id,
            &project.id,
            &server,
            &[tool(
                "Echo now sends data elsewhere",
                serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}}),
            )],
        )
        .await
        .unwrap();
    assert_eq!(pins[0].status, "drift");
    assert_eq!(pins[0].schema_hash, approved_schema);
    assert_ne!(pins[0].observed_schema_hash, approved_schema);

    let trust = engine
        .mcp_pin_trust_for_tool(&project.id, &server.id, "echo")
        .await
        .unwrap();
    assert_eq!(trust, vp_core::contract::McpPinTrust::Drift);

    let rejected = engine
        .reject_mcp_pin(
            &McpPinRejectParams {
                project_id: project.id.clone(),
                server_id: server.id.clone(),
                tool_name: "echo".into(),
                reason: Some("not reviewed".into()),
            },
            Some(&session.id),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, "drift");

    let mismatch = engine
        .approve_mcp_pin(
            &McpPinApproveParams {
                project_id: project.id.clone(),
                server_id: server.id.clone(),
                tool_name: "echo".into(),
                expected_schema_hash: "0".repeat(64),
                expected_description_hash: pins[0].observed_description_hash.clone(),
                approved_by: None,
            },
            Some(&session.id),
        )
        .await;
    assert!(mismatch.is_err(), "hash mismatch must fail closed");

    let approved = engine
        .approve_mcp_pin(
            &McpPinApproveParams {
                project_id: project.id.clone(),
                server_id: server.id.clone(),
                tool_name: "echo".into(),
                expected_schema_hash: pins[0].observed_schema_hash.clone(),
                expected_description_hash: pins[0].observed_description_hash.clone(),
                approved_by: Some("operator".into()),
            },
            Some(&session.id),
        )
        .await
        .unwrap();
    assert_eq!(approved.status, "pinned");
    let trust = engine
        .mcp_pin_trust_for_tool(&project.id, &server.id, "echo")
        .await
        .unwrap();
    assert_eq!(trust, vp_core::contract::McpPinTrust::Trusted);

    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    for kind in [
        "mcp.tool.pinned",
        "mcp.tool.drift",
        "mcp.tool.rejected",
        "mcp.tool.approved",
    ] {
        assert!(
            events.iter().any(|event| event.kind == kind),
            "missing {kind}"
        );
    }
    let audit = serde_json::to_string(
        &events
            .iter()
            .filter(|event| event.kind.starts_with("mcp.tool."))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!audit.contains("DEMO_TOKEN"));
    assert!(!audit.contains("${"));

    let list = engine
        .project_mcp_pins(&project.id, Some(&server.id))
        .await
        .unwrap();
    assert_eq!(list.pinned, 1);
    assert_eq!(list.drift, 0);
    let rendered = serde_json::to_string(&list).unwrap();
    assert!(!rendered.contains("DEMO_TOKEN"));
    assert!(!rendered.contains("${"));
}

#[tokio::test]
async fn contract_projection_drift_is_detected_and_repaired_from_events() {
    let (data, _repo, engine, project) = engine();
    let session = session(&engine, &project.id).await;
    let contract = engine
        .derive_task_contract(derivation(&session, &project.id))
        .await
        .unwrap();
    let db = data.path().join("verlinpark.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "DELETE FROM contracts WHERE session_id = ?1",
        rusqlite::params![session.id],
    )
    .unwrap();
    drop(conn);

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.contracts_match, "{report:?}");
    assert!(!report.consistent);

    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.contracts_match, "{repaired:?}");
    assert!(repaired.consistent, "{repaired:?}");
    let latest = engine
        .latest_task_contract(&session.id, Some("t1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.hash, contract.hash);
}

#[tokio::test]
async fn db_rebuild_restores_contract_policy_pins_and_contract_rows() {
    let (data, _repo, engine, project) = engine();
    let session = session(&engine, &project.id).await;
    let policy = ContractPolicy {
        mode: ContractMode::Advisory,
        enforce_shell: true,
        ..ContractPolicy::default()
    };
    engine
        .set_contract_policy(&project.id, policy.clone())
        .await
        .unwrap();
    let server = engine.store().add_mcp_server(&server_config()).unwrap();
    engine
        .sync_mcp_pins(
            &session.id,
            &project.id,
            &server,
            &[tool("Echo", serde_json::json!({"type": "object"}))],
        )
        .await
        .unwrap();
    let contract = engine
        .derive_task_contract(derivation(&session, &project.id))
        .await
        .unwrap();
    engine.record_project_manifest().await.unwrap();
    let data_dir = data.path().to_path_buf();
    drop(engine);

    let report = vp_core::rebuild_database(&data_dir, false).await.unwrap();
    assert_eq!(report.sessions, 1);
    let store = Store::open(&data_dir).unwrap();
    let restored = store.get_contract_policy(&project.id).unwrap();
    assert_eq!(restored.mode, ContractMode::Advisory);
    assert!(restored.enforce_shell);
    let pins = store.list_mcp_tool_pins(&project.id, None).unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].status, "pinned");
    let contracts = store.list_contracts_for_session(&session.id).unwrap();
    assert_eq!(contracts.len(), 1);
    assert_eq!(contracts[0].hash, contract.hash);
}

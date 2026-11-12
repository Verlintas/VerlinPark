//! End-to-end L1 instruction-store tests through the real agent loop, store,
//! export, replay, and database rebuild paths.
//!
//! These complement the unit tests in `vp_core::instruction`: they prove that
//! a real Leader run materializes ordered metadata, that a bundle carries the
//! exact verified blobs, that `session.instructions` never exposes text, and
//! that an event-mirror rebuild restores the state without re-running an agent.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{ApprovalMode, RoleModels};
use vp_providers::{ChatRequest, EventStream, Provider, ProviderError, StreamEvent};
use vp_store::Store;

struct TextProvider;

#[async_trait]
impl Provider for TextProvider {
    fn id(&self) -> &str {
        "text"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::Done),
        ])))
    }
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "instruction store integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "instruction-test@example.test"]);
    run(&["config", "user.name", "Instruction Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

fn test_service(data_dir: &Path) -> (Engine, AgentService) {
    let store = Store::open(data_dir).unwrap();
    let engine = Engine::new(store);
    let config = AgentConfig {
        provider: Arc::new(TextProvider),
        provider_name: "openai-compatible".to_string(),
        model: "test-model".to_string(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 2,
        max_tokens: Some(256),
        max_managers: 2,
        max_workers: 2,
        max_labour: 2,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 2,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(4, 4)),
        provider_idle_timeout_secs: 30,
        question_timeout_secs: 60,
        approval_mode: ApprovalMode::default(),
        approval_timeout_secs: 60,
        compact_after_tokens: 0,
        compact_keep_messages: 8,
        compact_keep_tokens: 1_000_000,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    };
    let service = AgentService::configured(engine.clone(), config);
    (engine, service)
}

async fn wait_for_idle(engine: &Engine, session_id: &str) {
    for _ in 0..300 {
        let session = engine
            .store()
            .get_session(session_id)
            .unwrap()
            .expect("session exists");
        if matches!(
            session.status.as_str(),
            "idle" | "failed" | "cancelled" | "paused"
        ) {
            assert_ne!(session.status, "failed", "agent run failed");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("session {session_id} did not become idle");
}

async fn run_leader(
    dir: &TempDir,
    goal: &str,
    prompt: &str,
) -> (Engine, AgentService, String, String) {
    let data_dir = dir.path().join("data");
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (engine, service) = test_service(&data_dir);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("instructions"))
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), goal.to_string(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    service.start(&session.id, prompt).await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    engine.record_project_manifest().await.unwrap();
    (engine, service, project.id, session.id)
}

#[tokio::test]
async fn leader_run_materializes_ordered_metadata_without_exposing_text() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, _project, session_id) =
        run_leader(&dir, "stable goal", "do the work").await;
    let report = engine
        .session_instructions(&session_id, None)
        .await
        .unwrap();
    assert_eq!(report.prompt_layout, 3);
    assert_eq!(report.states.len(), 1);
    let leader = &report.states[0];
    assert_eq!(leader.state_id, session_id);
    assert_eq!(leader.role.as_deref(), Some("leader"));
    let keys = leader
        .sections
        .iter()
        .map(|section| section.key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec!["core/charter", "core/recipes", "session/context",]
    );
    assert!(leader.sections.iter().all(|section| !section.tail));
    assert!(leader.sections.iter().all(|section| section.available));
    assert!(leader.sections.iter().all(|section| section.chars > 0));
    assert!(leader.head_hash.len() == 64);
    assert!(leader.tail_hash.is_empty());
    assert_eq!(
        report.latest_stable_prefix_hash.as_deref(),
        Some(leader.head_hash.as_str()),
        "stable prefix hash and the exact head text hash must agree"
    );
    assert_ne!(
        report.latest_prefix_hash.as_deref(),
        Some(leader.head_hash.as_str()),
        "prefix identity also covers model/tools, so it is not the raw text hash"
    );
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("stable goal"));
    assert!(!encoded.contains("You are the Leader"));
    assert!(!encoded.contains("do the work"));
    assert!(!encoded.contains("Role charter"));

    let events = engine
        .list_events(session_id.clone(), 0, 500)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .unwrap();
    assert_eq!(prefix.payload["prompt_layout"], 3);
    assert_eq!(prefix.payload["cache_epoch"], prefix.payload["prefix_hash"]);
    assert_eq!(prefix.payload["cache_epoch_source"], "l1_placeholder");
    assert_eq!(prefix.payload["cache_epoch_placeholder"], true);
    assert!(
        prefix.payload["section_chars"]["core/charter"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(prefix.payload["head_sections"].as_array().unwrap().len() >= 3);
    assert!(
        prefix.payload["tail_sections"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!prefix.payload.to_string().contains("stable goal"));
}

#[tokio::test]
async fn export_bundle_carries_verified_blobs_and_read_only_state() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, _project, session_id) =
        run_leader(&dir, "bundle goal", "bundle prompt").await;
    let bundle = engine.export_session(&session_id).await.unwrap();
    assert!(!bundle.instructions.is_empty());
    assert!(!bundle.instruction_blobs.is_empty());
    let hashes = bundle
        .instructions
        .iter()
        .map(|row| row.hash.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let blobs = bundle
        .instruction_blobs
        .iter()
        .map(|blob| blob.hash.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(hashes.is_subset(&blobs));
    for blob in &bundle.instruction_blobs {
        assert_eq!(blob.hash.len(), 64);
        let actual = vp_core::instruction::hash_instruction_text(&blob.text);
        assert_eq!(actual, blob.hash);
    }
    for row in &bundle.instructions {
        assert_eq!(row.session_id, session_id);
        assert!(!row.key.is_empty());
        assert!(row.ordinal >= 10);
    }
}

#[tokio::test]
async fn replay_detects_lost_projection_and_repairs_it_from_events() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, _project, session_id) =
        run_leader(&dir, "replay goal", "replay prompt").await;
    let before = engine
        .store()
        .list_session_instructions(&session_id)
        .unwrap();
    assert!(!before.is_empty());
    let clean = engine.replay_session(&session_id).await.unwrap();
    assert!(clean.consistent, "{:?}", clean.mismatches);

    engine
        .store()
        .replace_session_instructions(&session_id, &[])
        .unwrap();
    let drift = engine.replay_session(&session_id).await.unwrap();
    assert!(!drift.consistent);
    assert!(
        drift
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("instruction state:"))
    );
    engine.repair_session(&session_id).await.unwrap();
    let after = engine
        .store()
        .list_session_instructions(&session_id)
        .unwrap();
    assert_eq!(before.len(), after.len());
    for (left, right) in before.iter().zip(after.iter()) {
        assert_eq!(left.key, right.key);
        assert_eq!(left.hash, right.hash);
        assert_eq!(left.ordinal, right.ordinal);
        assert_eq!(left.tail, right.tail);
    }
    let report = engine.replay_session(&session_id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
}

#[tokio::test]
async fn session_cache_plan_is_read_only_and_does_not_create_instruction_state() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (engine, service) = test_service(&data_dir);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("preview"))
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "preview goal".to_string(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty()
    );
    let plan = service
        .cache_plan_now(&session.id, &["leader".to_string()])
        .await
        .unwrap();
    assert_eq!(plan.roles.len(), 1);
    assert_eq!(plan.roles[0].prompt_layout, 3);
    assert!(!plan.roles[0].head_sections.is_empty());
    assert!(plan.roles[0].stable_prefix_hash.len() == 64);
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty(),
        "cache.plan must not materialize state"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.kind != "context.instruction"),
        "cache.plan must not emit instruction state events"
    );
}

#[tokio::test]
async fn database_rebuild_restores_instruction_state_from_the_event_mirror() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let session_id = {
        let (engine, service) = test_service(&data_dir);
        let project = engine
            .store()
            .add_project(repo.to_str().unwrap(), Some("rebuild"))
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "rebuild goal".to_string(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test-model")
            .await
            .unwrap();
        service.start(&session.id, "rebuild prompt").await.unwrap();
        wait_for_idle(&engine, &session.id).await;
        engine.record_project_manifest().await.unwrap();
        let before = engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap();
        assert!(!before.is_empty());
        // Drop the engine so the rebuild can take the database without a live
        // writer. The event mirror remains the source of truth.
        session.id
    };

    let report = vp_core::rebuild::rebuild_database(&data_dir, false)
        .await
        .unwrap();
    assert!(report.sessions >= 1, "{report:?}");

    let store = Store::open(&data_dir).unwrap();
    let rows = store.list_session_instructions(&session_id).unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().any(|row| row.key == "core/charter"));
    assert!(rows.iter().any(|row| row.key == "session/context"));
    assert!(rows.iter().all(|row| row.hash.len() == 64));
    let engine = Engine::new(store);
    let report = engine
        .session_instructions(&session_id, None)
        .await
        .unwrap();
    assert_eq!(report.prompt_layout, 3);
    assert!(!report.states.is_empty());
}

#[tokio::test]
async fn instructions_rpc_isolates_task_scoped_state_and_filters_by_agent() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, _project, session_id) =
        run_leader(&dir, "scoped goal", "scoped prompt").await;
    let leader_rows = engine
        .store()
        .list_session_instructions(&session_id)
        .unwrap();
    let scope_id = format!("{session_id}#worker-42");
    let worker_rows = leader_rows
        .iter()
        .map(|row| vp_protocol::types::SessionInstructionState {
            session_id: scope_id.clone(),
            ..row.clone()
        })
        .collect::<Vec<_>>();
    engine
        .store()
        .replace_session_instructions(&scope_id, &worker_rows)
        .unwrap();

    let all = engine
        .session_instructions(&session_id, None)
        .await
        .unwrap();
    assert_eq!(all.states.len(), 2);
    assert!(all.states.iter().any(|state| state.state_id == session_id));
    assert!(all.states.iter().any(|state| state.state_id == scope_id));

    let filtered = engine
        .session_instructions(&session_id, Some("worker-42"))
        .await
        .unwrap();
    assert_eq!(filtered.states.len(), 1);
    assert_eq!(filtered.states[0].state_id, scope_id);
    assert_eq!(filtered.states[0].agent_id.as_deref(), Some("worker-42"));
    assert!(filtered.states[0].role.is_none());
    assert_eq!(
        filtered.states[0].head_hash, all.states[0].head_hash,
        "scoped state shares the same ordered leader bytes in this fixture"
    );

    let leader_only = engine
        .session_instructions(&session_id, Some("leader"))
        .await
        .unwrap();
    assert_eq!(leader_only.states.len(), 1);
    assert_eq!(leader_only.states[0].state_id, session_id);
}

//! End-to-end L4 cache-epoch tests through the real agent loop, store,
//! replay/repair, and database rebuild paths.
//!
//! The unit tests in `vp_core::cache_epoch` cover identity math. These tests
//! prove the projection survives a real Leader run, that replay detects a
//! removed observation/aggregate, that repair restores it from the event
//! stream, and that `db-rebuild` reconstructs it from event mirrors without a
//! second agent run or a provider call.

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

struct CachedProvider;

#[async_trait]
impl Provider for CachedProvider {
    fn id(&self) -> &str {
        "cached"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::Usage {
                input_tokens: 2_000,
                output_tokens: 4,
                cache_read_tokens: 1_800,
                cache_write_tokens: 0,
            }),
            Ok(StreamEvent::Done),
        ])))
    }
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "cache epoch integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "cache-epoch@example.test"]);
    run(&["config", "user.name", "Cache Epoch Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

fn test_service(data_dir: &Path) -> (Engine, AgentService) {
    let store = Store::open(data_dir).unwrap();
    let engine = Engine::new(store);
    let config = AgentConfig {
        provider: Arc::new(CachedProvider),
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
        max_active_sessions: 4,
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
        context_window: 128_000,
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

async fn setup(dir: &TempDir) -> (Engine, AgentService, String, String, std::path::PathBuf) {
    let data_dir = dir.path().join("data");
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (engine, service) = test_service(&data_dir);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("cache epochs"))
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "epoch goal".to_string(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    service.start(&session.id, "do the work").await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    engine.record_project_manifest().await.unwrap();
    (engine, service, project.id, session.id, data_dir)
}

#[tokio::test]
async fn leader_run_produces_final_epoch_and_one_observation() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project_id, session_id, _data) = setup(&dir).await;

    let events = engine
        .list_events(session_id.clone(), 0, 500)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix");
    let epoch = prefix.payload["cache_epoch"]
        .as_str()
        .expect("final epoch")
        .to_string();
    assert_eq!(epoch.len(), 64);
    assert_eq!(prefix.payload["cache_epoch_source"], "l4");
    assert_eq!(prefix.payload["cache_epoch_placeholder"], false);
    assert!(prefix.payload["cache_epoch_model"].as_str().is_some());
    assert!(prefix.payload["cache_epoch_tools_hash"].as_str().is_some());
    assert_ne!(
        prefix.payload["cache_epoch"], prefix.payload["prefix_hash"],
        "the shareable epoch is not the full head-prefix hash"
    );

    let cache = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache.payload["cache_epoch"], epoch);
    assert_eq!(cache.payload["warm"], false);
    assert_eq!(cache.payload["cross_session_first_call"], true);
    assert_eq!(cache.payload["cache_epoch_reused_from_sessions"], 0);

    let store = engine.store();
    let observations = store
        .list_prompt_prefix_observations(&project_id, &session_id)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].cache_epoch, epoch);
    assert_eq!(observations[0].calls, 1);
    assert_eq!(observations[0].observed_input_tokens, 2_000);
    assert_eq!(observations[0].observed_cache_read_tokens, 1_800);
    assert!(!observations[0].core_sections.is_empty());

    let aggregate = store
        .prompt_prefix_aggregate(&project_id, &epoch)
        .unwrap()
        .expect("aggregate");
    assert_eq!(aggregate.session_count, 1);
    assert_eq!(aggregate.call_count, 1);
    assert_eq!(aggregate.observed_input_tokens, 2_000);

    // Read-only inspection never mutates or calls the provider.
    let report = engine
        .session_cache_epoch(&session_id, Some(50))
        .await
        .unwrap();
    assert_eq!(report.total_epochs, 1);
    assert_eq!(report.prompt_layout, 3);
    assert_eq!(report.current_epoch.as_ref().unwrap().cache_epoch, epoch);
    assert!(report.generated_at.ends_with('Z'));
}

#[tokio::test]
async fn replay_detects_a_removed_prefix_projection_and_repair_rebuilds_it() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project_id, session_id, _data) = setup(&dir).await;
    let before = engine
        .session_cache_epoch(&session_id, Some(50))
        .await
        .unwrap();
    assert_eq!(before.total_epochs, 1);
    let epoch = before
        .current_epoch
        .as_ref()
        .expect("current epoch")
        .cache_epoch
        .clone();

    // Removing the observation also removes the derived aggregate. This is a
    // valid repair operation, not direct SQL, so the replay path is the only
    // thing under test.
    engine
        .store()
        .replace_prompt_prefix_observations(&project_id, &session_id, &[])
        .unwrap();
    let removed = engine
        .session_cache_epoch(&session_id, Some(50))
        .await
        .unwrap();
    assert_eq!(removed.total_epochs, 0);

    let replay = engine.replay_session(&session_id).await.unwrap();
    assert!(!replay.consistent, "{replay:?}");
    assert!(
        replay
            .mismatches
            .iter()
            .any(|mismatch| mismatch.contains("prompt prefix")),
        "replay must name the missing epoch projection: {replay:?}"
    );

    let repaired = engine.repair_session(&session_id).await.unwrap();
    assert!(repaired.consistent, "{repaired:?}");
    let after = engine
        .session_cache_epoch(&session_id, Some(50))
        .await
        .unwrap();
    assert_eq!(after.total_epochs, 1);
    assert_eq!(after.current_epoch.as_ref().unwrap().cache_epoch, epoch);
    assert_eq!(after.current_epoch.as_ref().unwrap().session_count, 1);
    assert_eq!(after.current_epoch.as_ref().unwrap().call_count, 1);
}

#[tokio::test]
async fn db_rebuild_restores_prompt_prefixes_from_event_mirrors() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project_id, session_id, data_dir) = setup(&dir).await;
    let before = engine
        .session_cache_epoch(&session_id, Some(50))
        .await
        .unwrap();
    let epoch = before
        .current_epoch
        .as_ref()
        .expect("current epoch")
        .cache_epoch
        .clone();
    drop(engine);

    let report = vp_core::rebuild::rebuild_database(&data_dir, false)
        .await
        .unwrap();
    assert!(report.sessions >= 1, "{report:?}");

    let store = Store::open(&data_dir).unwrap();
    let observations = store
        .list_prompt_prefix_observations(&project_id, &session_id)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].cache_epoch, epoch);
    assert_eq!(observations[0].calls, 1);
    let aggregate = store
        .prompt_prefix_aggregate(&project_id, &epoch)
        .unwrap()
        .expect("rebuilt aggregate");
    assert_eq!(aggregate.session_count, 1);

    let engine = Engine::new(store);
    let after = engine
        .session_cache_epoch(&session_id, Some(50))
        .await
        .unwrap();
    assert_eq!(after.total_epochs, 1);
    assert_eq!(after.current_epoch.as_ref().unwrap().cache_epoch, epoch);
    let replay = engine.replay_session(&session_id).await.unwrap();
    assert!(replay.consistent, "{replay:?}");
}

#[tokio::test]
async fn second_session_with_same_epoch_reports_reuse_and_aggregate() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project_id, first_id, _data) = setup(&dir).await;
    let second = engine
        .create_session(project_id.clone(), "another goal".to_string(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&second.id, "test-model")
        .await
        .unwrap();
    service.start(&second.id, "another prompt").await.unwrap();
    wait_for_idle(&engine, &second.id).await;

    let events = engine.list_events(second.id.clone(), 0, 500).await.unwrap();
    let cache = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache event");
    assert_eq!(cache.payload["cache_epoch_reused_from_sessions"], 1);
    assert_eq!(cache.payload["cross_session_first_call"], true);
    assert_eq!(cache.payload["cross_session_cache_read_tokens"], 1_800);
    assert_eq!(cache.payload["cache_epoch_session_count"], 2);

    let status = engine.session_cache_status(&second.id).await.unwrap();
    assert_eq!(status.prefix_reused_from_sessions, 1);
    assert_eq!(status.cross_session_hit_tokens, 1_800);
    assert_eq!(status.cache_epoch_session_count, 2);
    assert_eq!(status.cache_epoch_call_count, 2);
    assert!(!status.cache_epoch_core_sections.is_empty());

    let report = engine
        .session_cache_epoch(&second.id, Some(100))
        .await
        .unwrap();
    assert_eq!(report.total_epochs, 1);
    assert_eq!(report.epochs[0].session_count, 2);
    assert_eq!(report.epochs[0].call_count, 2);
    assert_eq!(report.epochs[0].this_session_calls, 1);
    assert_eq!(report.prefix_reused_from_sessions, 1);
    assert_eq!(report.reused_sessions.len(), 1);
    assert_eq!(report.reused_sessions[0].session_id, first_id);
    assert_eq!(report.reused_sessions[0].title, "epoch goal");
    assert_eq!(report.reused_sessions[0].calls, 1);
    assert!(report.reused_sessions[0].cache_hit_ratio > 0.8);

    let project_report = engine
        .project_cache_epochs(&project_id, Some(10))
        .await
        .unwrap();
    assert_eq!(project_report.total_epochs, 1);
    assert_eq!(project_report.total_sessions, 2);
    assert_eq!(project_report.total_calls, 2);
    assert_eq!(project_report.epochs.len(), 1);
    assert_eq!(project_report.epochs[0].session_count, 2);
    assert!((project_report.cache_hit_ratio - 0.9).abs() < 1e-9);
    assert_eq!(project_report.bundles.len(), 1);
    assert_eq!(project_report.bundles[0].tool_bundle, "leader");
    assert_eq!(project_report.bundles[0].sessions, 2);
    assert_eq!(project_report.bundles[0].calls, 2);
    assert!(project_report.generated_at.ends_with('Z'));
}

#[tokio::test]
async fn legacy_l1_prefix_events_do_not_fabricate_an_epoch_status() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (engine, _service) = test_service(&data_dir);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("legacy"))
        .unwrap();
    let session = engine
        .create_session(project.id, "legacy goal".into(), None)
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "context.prompt_prefix",
            serde_json::json!({
                "version": 3,
                "prompt_layout": 3,
                "prefix_hash": "a".repeat(64),
                "cache_epoch": "a".repeat(64),
                "cache_epoch_source": "l1_placeholder",
                "cache_epoch_placeholder": true,
                "stable_prefix_hash": "b".repeat(64),
            }),
        )
        .await
        .unwrap();
    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert!(status.cache_epoch.is_none());
    assert!(status.cache_epoch_source.is_none());
    assert_eq!(status.cache_epoch_session_count, 0);
    let epoch = engine
        .session_cache_epoch(&session.id, Some(10))
        .await
        .unwrap();
    assert!(epoch.current_epoch.is_none());
    assert_eq!(epoch.total_epochs, 0);
    assert_eq!(epoch.timeline.len(), 1);
    assert!(epoch.timeline[0].cache_epoch.is_none());
}

#[tokio::test]
async fn export_bundle_carries_epoch_observations_and_import_restores_them() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project_id, session_id, _data) = setup(&dir).await;
    let bundle = engine.export_session(&session_id).await.unwrap();
    assert_eq!(bundle.prompt_prefix_observations.len(), 1);
    let epoch = bundle.prompt_prefix_observations[0].cache_epoch.clone();
    assert_eq!(bundle.prompt_prefix_observations[0].session_id, session_id);
    assert_eq!(bundle.prompt_prefix_observations[0].calls, 1);
    drop(engine);

    let import_dir = TempDir::new().unwrap();
    let data_dir = import_dir.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    store.import_session_bundle(&bundle).unwrap();
    let observations = store
        .list_prompt_prefix_observations(&project_id, &session_id)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].cache_epoch, epoch);
    let aggregate = store
        .prompt_prefix_aggregate(&project_id, &epoch)
        .unwrap()
        .expect("imported aggregate");
    assert_eq!(aggregate.session_count, 1);
    let engine = Engine::new(store);
    let report = engine
        .session_cache_epoch(&session_id, Some(10))
        .await
        .unwrap();
    assert_eq!(report.total_epochs, 1);
    assert_eq!(report.current_epoch.as_ref().unwrap().cache_epoch, epoch);
    assert_eq!(report.current_epoch.as_ref().unwrap().session_count, 1);
}

#[tokio::test]
async fn db_rebuild_keeps_cross_session_epoch_counts_for_two_sessions() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project_id, first_id, data_dir) = setup(&dir).await;
    let second = engine
        .create_session(project_id.clone(), "second goal".to_string(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&second.id, "test-model")
        .await
        .unwrap();
    service.start(&second.id, "second prompt").await.unwrap();
    wait_for_idle(&engine, &second.id).await;
    engine.record_project_manifest().await.unwrap();
    let before = engine
        .project_cache_epochs(&project_id, Some(10))
        .await
        .unwrap();
    assert_eq!(before.total_epochs, 1);
    assert_eq!(before.total_sessions, 2);
    assert_eq!(before.total_calls, 2);
    assert_eq!(before.epochs[0].session_count, 2);
    drop(engine);

    let report = vp_core::rebuild::rebuild_database(&data_dir, false)
        .await
        .unwrap();
    assert!(report.sessions >= 2, "{report:?}");
    let engine = Engine::new(Store::open(&data_dir).unwrap());
    let after = engine
        .project_cache_epochs(&project_id, Some(10))
        .await
        .unwrap();
    assert_eq!(after.total_epochs, 1);
    assert_eq!(after.total_sessions, 2);
    assert_eq!(after.total_calls, 2);
    assert_eq!(after.epochs[0].session_count, 2);
    assert_eq!(after.epochs[0].call_count, 2);
    assert!((after.cache_hit_ratio - 0.9).abs() < 1e-9);
    let first_report = engine
        .session_cache_epoch(&first_id, Some(10))
        .await
        .unwrap();
    assert_eq!(first_report.total_epochs, 1);
    assert_eq!(
        first_report.reused_sessions.len(),
        1,
        "the rebuilt projection keeps the other session in the reuse list"
    );
    assert_eq!(first_report.reused_sessions[0].session_id, second.id);
}

#[tokio::test]
async fn partial_provider_hit_on_a_known_epoch_is_attributed_to_the_provider() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let (engine, service) = test_service(&data_dir);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("partial"))
        .unwrap();
    let first = engine
        .create_session(project.id.clone(), "one".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&first.id, "test-model")
        .await
        .unwrap();
    service.start(&first.id, "one").await.unwrap();
    wait_for_idle(&engine, &first.id).await;
    let second = engine
        .create_session(project.id.clone(), "two".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&second.id, "test-model")
        .await
        .unwrap();
    service.start(&second.id, "two").await.unwrap();
    wait_for_idle(&engine, &second.id).await;
    let cache = engine
        .list_events(second.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache event");
    // CachedProvider reports 1,800 of 2,000 input tokens, a 90% hit.
    assert_eq!(cache.payload["miss_reason"], serde_json::Value::Null);
    let status = engine.session_cache_status(&second.id).await.unwrap();
    assert_eq!(status.provider_cache_misses, 0);
    assert_eq!(status.cross_session_hit_tokens, 1_800);
    assert_eq!(status.prefix_changes, 0);
}

#[tokio::test]
async fn cache_audit_reports_cross_session_reuse_without_failing_the_session() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project_id, first_id, _data) = setup(&dir).await;
    let second = engine
        .create_session(project_id.clone(), "second goal".to_string(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&second.id, "test-model")
        .await
        .unwrap();
    service.start(&second.id, "second prompt").await.unwrap();
    wait_for_idle(&engine, &second.id).await;

    let audit = engine.session_cache_audit(&second.id).await.unwrap();
    assert!(
        audit
            .findings
            .iter()
            .any(|finding| finding.code == "cross_session_prefix_reuse"),
        "audit must surface provider-backed reuse: {audit:?}"
    );
    assert!(audit.clean);
    assert_eq!(audit.calls_checked, 1);
    let first_audit = engine.session_cache_audit(&first_id).await.unwrap();
    assert!(
        first_audit.findings.iter().all(|finding| {
            finding.code != "cross_session_prefix_reuse"
                || finding.evidence["cache_read_tokens"].as_u64().unwrap_or(0) == 0
        }),
        "the first session did not reuse another session's provider cache"
    );
}

#[tokio::test]
async fn project_epoch_prune_is_explicit_bounded_and_dry_run_safe() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project_id, session_id, _data) = setup(&dir).await;
    let before = engine
        .project_cache_epochs(&project_id, Some(10))
        .await
        .unwrap();
    assert_eq!(before.total_epochs, 1);

    let dry = engine
        .prune_project_cache_epochs(&project_id, Some(3650), Some(1), true)
        .await
        .unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.project_id, project_id);
    assert_eq!(dry.kept_epochs, 1);
    assert_eq!(dry.removed_epochs, 0);
    assert_eq!(dry.remaining_epochs, 1);
    assert!(dry.cutoff.ends_with('Z'));
    let after = engine
        .project_cache_epochs(&project_id, Some(10))
        .await
        .unwrap();
    assert_eq!(after.total_epochs, 1);
    let status = engine.session_cache_status(&session_id).await.unwrap();
    assert!(status.cache_epoch.is_some());

    // The clamp defaults are deterministic and never remove the newest epoch.
    let clamped = engine
        .prune_project_cache_epochs(&project_id, Some(0), Some(0), true)
        .await
        .unwrap();
    assert_eq!(clamped.kept_epochs, 1);
    assert_eq!(clamped.removed_epochs, 0);
}

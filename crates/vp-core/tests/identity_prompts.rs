//! S3 project-identity prompt and cache integration tests.
//!
//! These exercise the real `Engine` + `AgentService` prompt path with a fake
//! provider. They assert that an active identity assignment produces a stable
//! `agent/identity` head section, that a generation change moves the prefix and
//! cache epoch, that identity/department memory stays in the tail, and that
//! legacy/task-scoped roles keep the pre-S3 bytes. No network call is made and
//! no prompt text is read back from telemetry.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{
    AgentIdentityPolicy, ApprovalMode, CreateAgentIdentityParams, RoleModels, SessionAssignParams,
    UpdateAgentIdentityParams,
};
use vp_providers::{ChatRequest, EventStream, Provider, ProviderError, StreamEvent};
use vp_store::Store;

struct RecordingProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
}

impl RecordingProvider {
    fn new(responses: Vec<Vec<StreamEvent>>) -> (Arc<Self>, Arc<Mutex<Vec<ChatRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(Self {
                requests: requests.clone(),
                responses: Mutex::new(VecDeque::from(responses)),
            }),
            requests,
        )
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done]);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "identity prompt integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "identity-prompt@example.test"]);
    run(&["config", "user.name", "Identity Prompt Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

fn test_service(
    data_dir: &Path,
    provider: Arc<dyn Provider>,
) -> (Engine, AgentService, vp_protocol::types::Project) {
    test_service_with_compaction(data_dir, provider, 8, 1_000_000)
}

fn test_service_with_compaction(
    data_dir: &Path,
    provider: Arc<dyn Provider>,
    compact_keep_messages: usize,
    compact_keep_tokens: u64,
) -> (Engine, AgentService, vp_protocol::types::Project) {
    let store = Store::open(data_dir).unwrap();
    let engine = Engine::new(store);
    let repo = data_dir.join("repo");
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("identity prompts"))
        .unwrap();
    let config = AgentConfig {
        provider,
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
        compact_keep_messages,
        compact_keep_tokens,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    };
    let service = AgentService::configured(engine.clone(), config);
    (engine, service, project)
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

async fn enable_identity_policy(engine: &Engine, project_id: &str) {
    engine
        .set_agent_identity_policy(
            project_id,
            AgentIdentityPolicy {
                enabled: true,
                ..AgentIdentityPolicy::default()
            },
        )
        .await
        .unwrap();
}

async fn create_identity(engine: &Engine, project_id: &str, role: &str, title: &str) -> String {
    engine
        .create_agent_identity(CreateAgentIdentityParams {
            project_id: project_id.to_string(),
            role: role.to_string(),
            title: Some(title.to_string()),
            department: Some("Platform".to_string()),
            component: Some("auth".to_string()),
            profile: Some("coding".to_string()),
            model: Some("test-model".to_string()),
            ..CreateAgentIdentityParams::default()
        })
        .await
        .unwrap()
        .identity
        .id
}

async fn assign(engine: &Engine, session_id: &str, identity_id: &str) {
    engine
        .assign_session_identity(SessionAssignParams {
            session_id: session_id.to_string(),
            identity_id: identity_id.to_string(),
            ..SessionAssignParams::default()
        })
        .await
        .unwrap();
}

fn head_section<'a>(request: &'a ChatRequest, key: &str) -> Option<&'a str> {
    request
        .system_sections
        .iter()
        .find(|section| section.kind == key)
        .map(|section| section.text.as_str())
}

fn head_keys(request: &ChatRequest) -> Vec<&str> {
    request
        .system_sections
        .iter()
        .map(|section| section.kind.as_str())
        .collect()
}

fn request_head_text(request: &ChatRequest) -> String {
    request
        .system_sections
        .iter()
        .map(|section| format!("{}:{}", section.kind, section.text))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

fn request_tail_text(request: &ChatRequest) -> String {
    request
        .tail_sections
        .iter()
        .map(|section| format!("{}:{}", section.kind, section.text))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

async fn run_leader(engine: &Engine, service: &AgentService, session_id: &str, prompt: &str) {
    service.start(session_id, prompt).await.unwrap();
    wait_for_idle(engine, session_id).await;
}

#[tokio::test]
async fn legacy_session_without_assignment_has_no_identity_section() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![vec![
        StreamEvent::TextDelta("legacy done".into()),
        StreamEvent::Done,
    ]]);
    let (engine, service, project) = test_service(dir.path(), provider);
    let session = engine
        .create_session(project.id, "legacy goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    run_leader(&engine, &service, &session.id, "legacy prompt").await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert!(!head_keys(&captured[0]).contains(&"identity"));
    assert!(!head_keys(&captured[0]).contains(&"agent/identity"));
    assert!(head_section(&captured[0], "identity").is_none());
    assert_eq!(request_tail_text(&captured[0]), "");

    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    assert!(report.latest_identity_id.is_none());
    assert!(report.latest_identity_generation.is_none());
    assert!(report.latest_identity_hash.is_none());
    assert!(
        report
            .states
            .iter()
            .all(|state| state.identity_hash.is_none())
    );
    assert!(
        report.states[0]
            .sections
            .iter()
            .all(|section| { section.key != "agent/identity" && section.key != "agent/memory" })
    );
}

#[tokio::test]
async fn assigned_leader_identity_is_stable_across_runs() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id, "identity goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;

    run_leader(&engine, &service, &session.id, "first prompt").await;
    run_leader(&engine, &service, &session.id, "second prompt").await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let first = head_section(&captured[0], "identity").expect("identity section");
    let second = head_section(&captured[1], "identity").expect("identity section");
    assert_eq!(first, second, "same generation must be byte-identical");
    assert!(first.contains("Role: leader"));
    assert!(first.contains("Title: Project Lead"));
    assert!(first.contains("Department: Platform"));
    assert!(first.contains("Component: auth"));
    assert!(first.contains("Profile: coding"));
    assert!(first.contains("Model: test-model"));
    assert!(first.contains("Generation: 1"));
    assert!(first.contains("project and portfolio owner"));
    assert!(first.contains("Decision rights:"));
    assert!(first.contains("Review duties:"));
    assert!(!request_head_text(&captured[0]).contains("first prompt"));
    assert!(!first.contains("identity goal"));
    assert!(!first.contains("Session goal:"));
    assert!(!first.contains("Worktree:"));

    let prefixes = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 10)
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 2);
    assert_eq!(prefixes[0].payload["identity_id"], identity_id);
    assert_eq!(prefixes[0].payload["identity_generation"], 1);
    assert_eq!(
        prefixes[0].payload["identity_hash"],
        prefixes[1].payload["identity_hash"]
    );
    assert_eq!(
        prefixes[0].payload["prefix_hash"],
        prefixes[1].payload["prefix_hash"]
    );
    assert_eq!(
        prefixes[1].payload["cache_epoch_change_reason"],
        "unchanged"
    );
    assert_eq!(prefixes[0].payload["identity_section_count"], 1);
    assert_eq!(prefixes[0].payload["identity_memory_present"], false);
    let identity_sections = prefixes[0].payload["identity_sections"]
        .as_array()
        .expect("identity sections");
    assert_eq!(identity_sections.len(), 1);
    assert_eq!(identity_sections[0]["key"], "agent/identity");
    assert_eq!(identity_sections[0]["ordinal"], 15);
    assert_eq!(identity_sections[0]["tail"], false);
    assert!(
        prefixes[0]
            .payload
            .get("cache_epoch_identity_hash")
            .and_then(|value| value.as_str())
            .is_some_and(|hash| hash.len() == 64)
    );
    // Telemetry carries hashes and metadata only.
    let encoded = prefixes[0].payload.to_string();
    assert!(!encoded.contains("Project Lead"));
    assert!(!encoded.contains("project and portfolio owner"));
    assert!(!encoded.contains("first prompt"));

    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    assert_eq!(
        report.latest_identity_id.as_deref(),
        Some(identity_id.as_str())
    );
    assert_eq!(report.latest_identity_generation, Some(1));
    assert_eq!(
        report.latest_identity_hash.as_deref(),
        Some(prefixes[1].payload["identity_hash"].as_str().unwrap())
    );
    let cache = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(
        cache.cache_epoch_identity_hash.as_deref(),
        Some(prefixes[1].payload["identity_hash"].as_str().unwrap())
    );
    assert_eq!(
        cache.cache_epoch_identity_id.as_deref(),
        Some(identity_id.as_str())
    );
    assert_eq!(cache.cache_epoch_identity_generation, Some(1));
    assert_eq!(cache.cache_epoch_identity_sections.len(), 1);
    assert_eq!(cache.cache_epoch_identity_sections[0].ordinal, 15);
    let state = &report.states[0];
    assert_eq!(
        state.identity_hash.as_deref(),
        report.latest_identity_hash.as_deref()
    );
    let keys = state
        .sections
        .iter()
        .map(|section| section.key.as_str())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"agent/identity"));
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("Project Lead"));
    assert!(!encoded.contains("project and portfolio owner"));
    assert!(!encoded.contains("identity goal"));
}

#[tokio::test]
async fn generation_change_moves_the_identity_and_the_cache_epoch() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id, "generation goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;

    run_leader(&engine, &service, &session.id, "first prompt").await;
    engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: identity_id.clone(),
            title: Some(Some("Portfolio Lead".to_string())),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    run_leader(&engine, &service, &session.id, "second prompt").await;

    let captured = requests.lock().unwrap().clone();
    let first = head_section(&captured[0], "identity").unwrap();
    let second = head_section(&captured[1], "identity").unwrap();
    assert_ne!(first, second);
    assert!(first.contains("Title: Project Lead"));
    assert!(second.contains("Title: Portfolio Lead"));
    assert!(second.contains("Generation: 2"));
    assert!(second.contains("Role: leader"));

    let prefixes = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 10)
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 2);
    assert_ne!(
        prefixes[0].payload["prefix_hash"],
        prefixes[1].payload["prefix_hash"]
    );
    assert_ne!(
        prefixes[0].payload["identity_hash"],
        prefixes[1].payload["identity_hash"]
    );
    assert_eq!(prefixes[1].payload["cache_epoch_change_reason"], "identity");
    assert_ne!(
        prefixes[0].payload["cache_epoch"],
        prefixes[1].payload["cache_epoch"]
    );
    assert_eq!(
        prefixes[0].payload["core_hash"],
        prefixes[1].payload["core_hash"]
    );
    assert_eq!(
        prefixes[0].payload["project_hash"],
        prefixes[1].payload["project_hash"]
    );
    assert_eq!(
        prefixes[0].payload["section_chars"]["core/charter"],
        prefixes[1].payload["section_chars"]["core/charter"]
    );
}

#[tokio::test]
async fn identity_memory_is_tail_only_and_never_moves_the_head() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id, "memory goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;

    run_leader(&engine, &service, &session.id, "first prompt").await;
    engine
        .create_memory(
            &session.id,
            "decision",
            "Auth component decision",
            "The auth component owns JWT validation and rotates signing keys on a schedule.",
            vec!["auth".into(), "platform".into()],
        )
        .await
        .unwrap();
    run_leader(&engine, &service, &session.id, "second prompt").await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        head_section(&captured[0], "identity"),
        head_section(&captured[1], "identity")
    );
    assert_eq!(
        request_head_text(&captured[0]),
        request_head_text(&captured[1])
    );
    assert!(!request_head_text(&captured[1]).contains("JWT validation"));
    assert!(request_tail_text(&captured[1]).contains("agent/memory:"));
    assert!(request_tail_text(&captured[1]).contains("JWT validation"));
    assert!(request_tail_text(&captured[1]).contains("Identity/department memory"));
    assert!(request_tail_text(&captured[1]).contains("not conversation and not authorization"));

    let prefixes = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 10)
        .await
        .unwrap();
    assert_eq!(
        prefixes[0].payload["prefix_hash"],
        prefixes[1].payload["prefix_hash"]
    );
    assert_eq!(
        prefixes[0].payload["identity_hash"],
        prefixes[1].payload["identity_hash"]
    );
    assert_ne!(
        prefixes[0].payload["volatile_hash"],
        prefixes[1].payload["volatile_hash"]
    );
    assert_eq!(prefixes[0].payload["identity_memory_present"], false);
    assert_eq!(prefixes[1].payload["identity_memory_present"], true);
    assert!(prefixes[0].payload["identity_memory"].is_null());
    let telemetry = &prefixes[1].payload["identity_memory"];
    assert_eq!(telemetry["source"], "legacy_project_memory");
    assert_eq!(telemetry["memory_count"], 1);
    assert!(
        telemetry["brief_chars"].as_u64().unwrap_or(0) > 0,
        "{telemetry:?}"
    );
    assert_eq!(
        telemetry["brief_hash"].as_str().unwrap_or_default().len(),
        64
    );
    assert!(
        telemetry["memory_hashes"]
            .as_array()
            .is_some_and(|hashes| hashes.len() == 1)
    );
    assert!(!telemetry.to_string().contains("JWT validation"));
    let tail_keys = prefixes[1].payload["tail_sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section["key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(tail_keys.contains(&"agent/memory"));

    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    let latest_memory = report
        .latest_identity_memory
        .as_ref()
        .expect("latest identity memory telemetry");
    assert_eq!(latest_memory.memory_count, 1);
    assert_eq!(latest_memory.source, "legacy_project_memory");
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("JWT validation"));
}

#[tokio::test]
async fn cache_plan_resolves_identity_for_leader_manager_and_worker_only() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(Vec::new());
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let leader = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let manager = create_identity(&engine, &project.id, "manager", "Platform Manager").await;
    let worker = create_identity(&engine, &project.id, "worker", "Auth Tech Lead").await;
    let session = engine
        .create_session(project.id.clone(), "cache plan goal".into(), None)
        .await
        .unwrap();
    assign(&engine, &session.id, &leader).await;
    assign(&engine, &session.id, &manager).await;
    assign(&engine, &session.id, &worker).await;

    let plan = service
        .cache_plan_now(
            &session.id,
            &[
                "leader".to_string(),
                "manager".to_string(),
                "worker".to_string(),
                "labour".to_string(),
                "verifier".to_string(),
                "scout".to_string(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(plan.roles.len(), 6);
    for role in ["leader", "manager", "worker"] {
        let entry = plan.roles.iter().find(|entry| entry.role == role).unwrap();
        assert!(
            entry
                .head_sections
                .iter()
                .any(|section| section.key == "agent/identity"),
            "{role} plan must include agent/identity: {:?}",
            entry
                .head_sections
                .iter()
                .map(|section| section.key.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(entry.identity_hash.len(), 64);
        assert_eq!(entry.identity_generation, Some(1));
        assert!(entry.identity_id.is_some());
    }
    for role in ["labour", "verifier", "scout"] {
        let entry = plan.roles.iter().find(|entry| entry.role == role).unwrap();
        assert!(
            entry
                .head_sections
                .iter()
                .all(|section| section.key != "agent/identity"),
            "{role} must not receive an identity section"
        );
        assert!(entry.identity_hash.is_empty());
        assert!(entry.identity_id.is_none());
        assert!(entry.identity_generation.is_none());
    }
}

#[tokio::test]
async fn multiple_worker_identities_resolve_by_department_then_age() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(Vec::new());
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let billing = create_identity(&engine, &project.id, "worker", "Billing Tech Lead").await;
    let auth = create_identity(&engine, &project.id, "worker", "Auth Tech Lead").await;
    let session = engine
        .create_session(project.id, "worker selection".into(), None)
        .await
        .unwrap();
    assign(&engine, &session.id, &billing).await;
    assign(&engine, &session.id, &auth).await;

    let plan = service
        .cache_plan_now(&session.id, &["worker".to_string()])
        .await
        .unwrap();
    let entry = &plan.roles[0];
    assert_eq!(entry.identity_id.as_deref(), Some(billing.as_str()));
    assert_eq!(entry.identity_generation, Some(1));

    let resolved = engine
        .prompt_identity_for_run(&session.id, "worker", Some("Auth Tech Lead"), None)
        .await
        .unwrap()
        .expect("title match");
    assert_eq!(resolved.id, auth);
}

#[tokio::test]
async fn metadata_only_identity_changes_never_leak_into_session_reads() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![vec![
        StreamEvent::TextDelta("done".into()),
        StreamEvent::Done,
    ]]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Secret Title").await;
    let session = engine
        .create_session(project.id.clone(), "metadata goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    run_leader(&engine, &service, &session.id, "metadata prompt").await;

    let captured = requests.lock().unwrap().clone();
    assert!(
        head_section(&captured[0], "identity")
            .unwrap()
            .contains("Secret Title")
    );

    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("Secret Title"));
    assert!(!encoded.contains("Decision rights"));
    assert!(!encoded.contains("metadata prompt"));
    assert!(!encoded.contains("metadata goal"));
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert!(
        bundle
            .instructions
            .iter()
            .any(|row| row.key == "agent/identity")
    );
    let identity_row = bundle
        .instructions
        .iter()
        .find(|row| row.key == "agent/identity")
        .unwrap();
    // The archive intentionally carries the verified prompt blob. Ordinary
    // telemetry and `session.instructions` never do; the bundle is an explicit
    // session export, not a cache/telemetry surface.
    assert!(
        bundle
            .instruction_blobs
            .iter()
            .any(|blob| blob.hash == identity_row.hash
                && vp_core::instruction::hash_instruction_text(&blob.text) == blob.hash)
    );
    assert!(identity_row.key == "agent/identity");
    assert!(identity_row.hash.len() == 64);
}

#[tokio::test]
async fn project_boundary_and_legacy_sessions_stay_unchanged() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(Vec::new());
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;

    let other_repo = TempDir::new().unwrap();
    init_repo(other_repo.path());
    let other_project = engine
        .store()
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    let other_session = engine
        .store()
        .create_session(&other_project.id, "other goal")
        .unwrap();
    // A cross-project identity can never be assigned; the prompt resolver also
    // rejects a manually planted assignment whose identity project differs.
    let error = engine
        .assign_session_identity(SessionAssignParams {
            session_id: other_session.id.clone(),
            identity_id: identity_id.clone(),
            ..SessionAssignParams::default()
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("different projects"));

    let legacy = engine
        .create_session(project.id.clone(), "legacy goal".into(), None)
        .await
        .unwrap();
    assert!(
        engine
            .prompt_identity_for_run(&legacy.id, "leader", None, None)
            .await
            .unwrap()
            .is_none()
    );
    let report = engine.session_instructions(&legacy.id, None).await.unwrap();
    assert!(report.states.is_empty());
    let plan = service
        .cache_plan_now(&legacy.id, &["leader".to_string()])
        .await
        .unwrap();
    assert!(
        plan.roles[0]
            .head_sections
            .iter()
            .all(|section| section.key != "agent/identity")
    );
    assert!(plan.roles[0].identity_hash.is_empty());
}

#[tokio::test]
async fn identity_instruction_state_replays_and_repairs_after_projection_loss() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(vec![vec![
        StreamEvent::TextDelta("done".into()),
        StreamEvent::Done,
    ]]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id, "replay goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    run_leader(&engine, &service, &session.id, "replay prompt").await;

    let before = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let identity_row = before
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("identity row")
        .clone();
    assert_eq!(identity_row.ordinal, 15);
    assert!(!identity_row.tail);

    let clean = engine.replay_session(&session.id).await.unwrap();
    assert!(clean.consistent, "{:?}", clean.mismatches);

    engine
        .store()
        .replace_session_instructions(&session.id, &[])
        .unwrap();
    let drift = engine.replay_session(&session.id).await.unwrap();
    assert!(!drift.consistent);
    assert!(
        drift
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("instruction state:"))
    );
    engine.repair_session(&session.id).await.unwrap();
    let after = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let repaired = after
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("repaired identity row");
    assert_eq!(repaired.hash, identity_row.hash);
    assert_eq!(repaired.ordinal, 15);
    assert!(!repaired.tail);

    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    assert_eq!(
        report.latest_identity_id.as_deref(),
        Some(identity_id.as_str())
    );
    assert_eq!(
        report.states[0].identity_hash.as_deref(),
        Some(identity_row.hash.as_str())
    );
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("Project Lead"));
}

#[tokio::test]
async fn same_identity_generation_reuses_the_epoch_across_sessions() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let first_session = engine
        .create_session(project.id.clone(), "first identity session".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&first_session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &first_session.id, &identity_id).await;
    run_leader(&engine, &service, &first_session.id, "first prompt").await;

    let first_prefix = engine
        .recent_events_of_kind(&first_session.id, "context.prompt_prefix", 1)
        .await
        .unwrap()
        .pop()
        .expect("first prompt prefix");
    engine
        .release_session_identity(vp_protocol::types::SessionReleaseParams {
            session_id: first_session.id.clone(),
            identity_id: identity_id.clone(),
            operator: true,
            reason: Some("hand off to the next session".to_string()),
        })
        .await
        .unwrap();

    let second_session = engine
        .create_session(project.id.clone(), "second identity session".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&second_session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &second_session.id, &identity_id).await;
    run_leader(&engine, &service, &second_session.id, "second prompt").await;

    let second_prefix = engine
        .recent_events_of_kind(&second_session.id, "context.prompt_prefix", 1)
        .await
        .unwrap()
        .pop()
        .expect("second prompt prefix");
    assert_eq!(
        first_prefix.payload["identity_hash"],
        second_prefix.payload["identity_hash"]
    );
    assert_eq!(
        first_prefix.payload["cache_epoch"],
        second_prefix.payload["cache_epoch"]
    );
    assert_ne!(
        first_prefix.payload["prefix_hash"], second_prefix.payload["prefix_hash"],
        "session/context is part of the exact head but not the shareable epoch"
    );
    assert_ne!(
        first_prefix.payload["stable_prefix_hash"],
        second_prefix.payload["stable_prefix_hash"]
    );
    assert_ne!(
        first_prefix.payload["section_chars"]["session/context"],
        second_prefix.payload["section_chars"]["session/context"]
    );
    assert!(
        second_prefix.payload["cache_epoch_reused_from_sessions"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        head_section(&captured[0], "identity"),
        head_section(&captured[1], "identity"),
        "the same identity generation must be byte-identical across sessions"
    );
    assert!(
        !head_section(&captured[0], "identity")
            .unwrap()
            .contains("first identity session")
    );
    assert!(
        !head_section(&captured[1], "identity")
            .unwrap()
            .contains("second identity session")
    );
}

#[tokio::test]
async fn explicit_warm_uses_the_identity_head_and_reports_its_metadata() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("warm ok".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("run ok".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "warm identity goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;

    let warm = service.warm_cache_now(&session.id, None).await.unwrap();
    assert!(warm.warmed);
    assert_eq!(warm.identity_id.as_deref(), Some(identity_id.as_str()));
    assert_eq!(warm.identity_generation, Some(1));
    assert!(
        warm.identity_hash
            .as_deref()
            .is_some_and(|hash| hash.len() == 64)
    );
    assert!(warm.cache_epoch.is_some());

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert!(head_section(&captured[0], "identity").is_some());
    assert!(
        head_section(&captured[0], "identity")
            .unwrap()
            .contains("Project Lead")
    );
    assert!(captured[0].tail_sections.is_empty());
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .iter()
            .any(|row| row.key == "agent/identity")
    );
}

#[tokio::test]
async fn retried_identity_run_rebuilds_a_missing_identity_blob() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "blob rebuild goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    run_leader(&engine, &service, &session.id, "first prompt").await;

    let rows = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let identity_hash = rows
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("identity row")
        .hash
        .clone();
    assert!(
        engine
            .store()
            .delete_instruction_blob(&identity_hash)
            .unwrap()
    );
    run_leader(&engine, &service, &session.id, "second prompt").await;

    let rebuilt = engine
        .recent_events_of_kind(&session.id, "context.instruction.rebuilt", 20)
        .await
        .unwrap();
    assert!(rebuilt.iter().any(|event| {
        event.payload["key"] == "agent/identity"
            && event.payload["reason"] == "missing_blob"
            && event.payload["new_hash"] == identity_hash
    }));
    let repaired = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let identity_row = repaired
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("rebuilt identity row");
    assert_eq!(identity_row.hash, identity_hash);
    assert!(
        engine
            .store()
            .get_instruction_blob(&identity_hash)
            .unwrap()
            .is_some()
    );
    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    assert_eq!(
        report.latest_identity_hash.as_deref(),
        Some(identity_hash.as_str())
    );
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("Project Lead"));
}

#[tokio::test]
async fn retired_identity_is_not_used_by_a_new_run() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "retired goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    assert!(
        engine
            .prompt_identity_for_run(&session.id, "leader", None, None)
            .await
            .unwrap()
            .is_some()
    );
    engine
        .retire_agent_identity(vp_protocol::types::RetireAgentIdentityParams {
            identity_id: identity_id.clone(),
            successor_id: None,
        })
        .await
        .unwrap();
    assert!(
        engine
            .prompt_identity_for_run(&session.id, "leader", None, None)
            .await
            .unwrap()
            .is_none()
    );
    let prefix = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 1)
        .await
        .unwrap();
    assert!(prefix.is_empty(), "no run was started after retirement");
    // The assignment row remains release-able even though the identity is
    // retired; S2 owns lease cleanup and S3 only stops using the identity in a
    // new prompt.
    let _ = service;
}

#[tokio::test]
async fn cache_plan_with_identity_does_not_materialize_instruction_state() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(Vec::new());
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let worker = create_identity(&engine, &project.id, "worker", "Auth Tech Lead").await;
    let session = engine
        .create_session(project.id, "plan only".into(), None)
        .await
        .unwrap();
    assign(&engine, &session.id, &worker).await;
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty()
    );
    let plan = service
        .cache_plan_now(&session.id, &["worker".to_string()])
        .await
        .unwrap();
    assert_eq!(plan.roles[0].identity_generation, Some(1));
    assert!(plan.roles[0].identity_hash.len() == 64);
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty(),
        "cache.plan must not persist instruction state"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.kind != "context.instruction")
    );
}

#[tokio::test]
async fn database_rebuild_restores_identity_instruction_state_from_the_event_mirror() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let (engine, service, project) = test_service(&data_dir, {
        let (provider, _requests) = RecordingProvider::new(vec![vec![
            StreamEvent::TextDelta("rebuild".into()),
            StreamEvent::Done,
        ]]);
        provider
    });
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "rebuild identity goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    engine
        .create_memory(
            &session.id,
            "decision",
            "Auth boundary",
            "The auth component owns token validation.",
            vec!["auth".into()],
        )
        .await
        .unwrap();
    run_leader(&engine, &service, &session.id, "rebuild prompt").await;
    engine.record_project_manifest().await.unwrap();
    let before = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let identity_hash = before
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("identity row before rebuild")
        .hash
        .clone();
    let prefix = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 1)
        .await
        .unwrap()
        .pop()
        .expect("prompt prefix before rebuild");
    let expected_identity_hash = prefix.payload["identity_hash"]
        .as_str()
        .expect("identity hash")
        .to_string();
    assert_eq!(identity_hash, expected_identity_hash);
    let session_id = session.id.clone();
    drop(service);
    drop(engine);

    let report = vp_core::rebuild::rebuild_database(&data_dir, false)
        .await
        .unwrap();
    assert!(report.sessions >= 1, "{report:?}");

    let store = Store::open(&data_dir).unwrap();
    let rows = store.list_session_instructions(&session_id).unwrap();
    let identity = rows
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("identity row after rebuild");
    assert_eq!(identity.hash, identity_hash);
    assert_eq!(identity.ordinal, 15);
    assert!(!identity.tail);
    assert!(rows.iter().any(|row| row.key == "agent/memory" && row.tail));
    let engine = Engine::new(store);
    let report = engine
        .session_instructions(&session_id, None)
        .await
        .unwrap();
    assert_eq!(
        report.latest_identity_hash.as_deref(),
        Some(identity_hash.as_str())
    );
    assert_eq!(
        report.latest_identity_id.as_deref(),
        Some(identity_id.as_str())
    );
    assert_eq!(report.latest_identity_generation, Some(1));
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("Project Lead"));
}

#[tokio::test]
async fn identity_memory_is_scoped_to_the_resolved_identity() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(Vec::new());
    let (engine, _service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let auth = create_identity(&engine, &project.id, "worker", "Auth Tech Lead").await;
    let billing = create_identity(&engine, &project.id, "worker", "Billing Tech Lead").await;
    engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: billing.clone(),
            component: Some(Some("billing".to_string())),
            title: Some(Some("Billing Tech Lead".to_string())),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "scoped memory goal".into(), None)
        .await
        .unwrap();
    assign(&engine, &session.id, &auth).await;
    assign(&engine, &session.id, &billing).await;
    engine
        .create_memory(
            &session.id,
            "decision",
            "Auth boundary",
            "The auth component validates bearer tokens before routing.",
            vec!["auth".into()],
        )
        .await
        .unwrap();
    engine
        .create_memory(
            &session.id,
            "decision",
            "Billing boundary",
            "The billing component owns invoice reconciliation and tax evidence.",
            vec!["billing".into()],
        )
        .await
        .unwrap();

    let resolved = engine
        .prompt_identity_for_run(&session.id, "worker", Some("Billing Tech Lead"), None)
        .await
        .unwrap()
        .expect("billing identity");
    assert_eq!(resolved.id, billing);
    let brief = engine
        .identity_memory_brief(&session.id, &resolved)
        .await
        .unwrap();
    assert!(brief.text.contains("billing component"));
    assert!(!brief.text.contains("auth component"));

    let resolved = engine
        .prompt_identity_for_run(&session.id, "worker", Some("Auth Tech Lead"), None)
        .await
        .unwrap()
        .expect("auth identity");
    let brief = engine
        .identity_memory_brief(&session.id, &resolved)
        .await
        .unwrap();
    assert!(brief.text.contains("auth component"));
    assert!(!brief.text.contains("billing component"));

    // The same brief is bounded before it can become a prompt tail.
    assert!(
        brief.text.chars().count()
            <= vp_core::identity_prompt::IDENTITY_MEMORY_BRIEF_CHARS as usize
    );
    assert_eq!(brief.source, "legacy_project_memory");
}

#[tokio::test]
async fn generation_change_is_attributed_as_an_identity_cache_break() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "miss attribution goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    run_leader(&engine, &service, &session.id, "first prompt").await;
    engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id,
            title: Some(Some("Portfolio Lead".to_string())),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    run_leader(&engine, &service, &session.id, "second prompt").await;

    let cache_events = engine
        .recent_events_of_kind(&session.id, "context.cache", 20)
        .await
        .unwrap();
    assert!(cache_events.iter().any(|event| {
        event.payload["miss_reason"] == "identity_changed"
            && event.payload["cache_epoch_source"] == "l4"
    }));
    assert!(cache_events.iter().any(|event| {
        event.payload["cache_epoch_identity_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
            && event.payload["cache_epoch_identity_generation"] == 2
    }));
    assert!(cache_events.iter().all(|event| {
        let encoded = event.payload.to_string();
        !encoded.contains("Project Lead")
            && !encoded.contains("Portfolio Lead")
            && !encoded.contains("Agent identity")
    }));

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert!(status.prefix_changes >= 1 || status.cache_epochs_seen >= 2);
    assert!(status.cache_epoch_identity_hash.is_some());
    assert_eq!(status.cache_epoch_identity_generation, Some(2));
    let encoded = serde_json::to_string(&status).unwrap();
    assert!(!encoded.contains("Portfolio Lead"));
    assert!(!encoded.contains("Project Lead"));
}

#[tokio::test]
async fn conversation_compaction_preserves_the_identity_head_section() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(vec![
        vec![
            StreamEvent::TextDelta("first answer".into()),
            StreamEvent::Done,
        ],
        vec![
            StreamEvent::TextDelta("second answer".into()),
            StreamEvent::Done,
        ],
        vec![
            StreamEvent::TextDelta("compacted summary".into()),
            StreamEvent::Done,
        ],
    ]);
    let (engine, service, project) = test_service_with_compaction(dir.path(), provider, 2, 1000);
    enable_identity_policy(&engine, &project.id).await;
    let identity_id = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "compaction goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity_id).await;
    run_leader(&engine, &service, &session.id, "first prompt").await;
    run_leader(&engine, &service, &session.id, "second prompt").await;
    run_leader(&engine, &service, &session.id, "third prompt").await;
    run_leader(&engine, &service, &session.id, "fourth prompt").await;

    let before = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let identity = before
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("identity row")
        .clone();
    assert_eq!(identity.ordinal, 15);
    assert!(!identity.tail);

    let compacted = service.compact_now(&session.id).await.unwrap();
    assert!(compacted.compacted, "manual compaction should run");

    let after = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let after_identity = after
        .iter()
        .find(|row| row.key == "agent/identity")
        .expect("identity row after compaction");
    assert_eq!(after_identity.hash, identity.hash);
    assert_eq!(after_identity.ordinal, 15);
    assert!(!after_identity.tail);
    assert_eq!(
        before.len(),
        after.len(),
        "compaction must not add or drop instruction sections"
    );
    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    assert_eq!(
        report.latest_identity_id.as_deref(),
        Some(identity_id.as_str())
    );
    assert_eq!(
        report.states[0].identity_hash.as_deref(),
        Some(identity.hash.as_str())
    );
    let events = engine
        .recent_events_of_kind(&session.id, "context.compacted", 5)
        .await
        .unwrap();
    assert!(!events.is_empty());
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("Project Lead"));
}

#[tokio::test]
async fn manager_title_and_worker_department_preferences_are_deterministic() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(Vec::new());
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let quality = create_identity(&engine, &project.id, "manager", "Quality").await;
    let discovery = create_identity(&engine, &project.id, "manager", "Discovery").await;
    let auth = create_identity(&engine, &project.id, "worker", "Auth Tech Lead").await;
    let billing = create_identity(&engine, &project.id, "worker", "Billing Tech Lead").await;
    engine
        .update_agent_identity(UpdateAgentIdentityParams {
            identity_id: billing.clone(),
            department: Some(Some("Finance".to_string())),
            component: Some(Some("billing".to_string())),
            ..UpdateAgentIdentityParams::default()
        })
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "selection goal".into(), None)
        .await
        .unwrap();
    assign(&engine, &session.id, &quality).await;
    assign(&engine, &session.id, &discovery).await;
    assign(&engine, &session.id, &auth).await;
    assign(&engine, &session.id, &billing).await;

    let selected = engine
        .prompt_identity_for_run(&session.id, "manager", Some("Discovery"), None)
        .await
        .unwrap()
        .expect("title match");
    assert_eq!(selected.id, discovery);
    let selected = engine
        .prompt_identity_for_run(&session.id, "manager", None, Some("Platform"))
        .await
        .unwrap()
        .expect("department fallback");
    assert_eq!(
        selected.id, quality,
        "oldest manager is the stable fallback"
    );

    let selected = engine
        .prompt_identity_for_run(&session.id, "worker", None, Some("Finance"))
        .await
        .unwrap()
        .expect("worker department match");
    assert_eq!(selected.id, billing);
    let selected = engine
        .prompt_identity_for_run(&session.id, "worker", None, None)
        .await
        .unwrap()
        .expect("oldest worker fallback");
    assert_eq!(selected.id, auth);

    // The read-only cache plan uses the same deterministic fallback (oldest),
    // while a real Manager run with a title selects Discovery.
    let plan = service
        .cache_plan_now(&session.id, &["manager".to_string()])
        .await
        .unwrap();
    assert_eq!(plan.roles[0].identity_id.as_deref(), Some(quality.as_str()));
}

#[tokio::test]
async fn identity_resolution_requires_a_matching_role_and_live_identity() {
    let dir = TempDir::new().unwrap();
    let (provider, _requests) = RecordingProvider::new(Vec::new());
    let (engine, _service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let leader = create_identity(&engine, &project.id, "leader", "Project Lead").await;
    let session = engine
        .create_session(project.id.clone(), "resolution goal".into(), None)
        .await
        .unwrap();

    for role in ["leader", "manager", "worker", "labour", "verifier", "scout"] {
        assert!(
            engine
                .prompt_identity_for_run(&session.id, role, None, None)
                .await
                .unwrap()
                .is_none(),
            "unassigned {role} must not resolve an identity"
        );
    }
    assign(&engine, &session.id, &leader).await;
    assert!(
        engine
            .prompt_identity_for_run(&session.id, "leader", None, None)
            .await
            .unwrap()
            .is_some()
    );
    for role in ["manager", "worker", "labour", "verifier", "scout"] {
        assert!(
            engine
                .prompt_identity_for_run(&session.id, role, None, None)
                .await
                .unwrap()
                .is_none(),
            "{role} must not borrow the leader identity"
        );
    }
    engine
        .pause_agent_identity(vp_protocol::types::PauseAgentIdentityParams {
            identity_id: leader.clone(),
        })
        .await
        .unwrap();
    assert!(
        engine
            .prompt_identity_for_run(&session.id, "leader", None, None)
            .await
            .unwrap()
            .is_none()
    );
    engine
        .resume_agent_identity(vp_protocol::types::ResumeAgentIdentityParams {
            identity_id: leader.clone(),
        })
        .await
        .unwrap();
    assert!(
        engine
            .prompt_identity_for_run(&session.id, "leader", None, None)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn worker_warm_cache_includes_identity_and_worker_delegation_schema() {
    let dir = TempDir::new().unwrap();
    let (provider, requests) = RecordingProvider::new(vec![
        vec![StreamEvent::TextDelta("warm".into()), StreamEvent::Done],
        vec![StreamEvent::TextDelta("run".into()), StreamEvent::Done],
    ]);
    let (engine, service, project) = test_service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let worker = create_identity(&engine, &project.id, "worker", "Auth Tech Lead").await;
    let session = engine
        .create_session(project.id.clone(), "worker warm goal".into(), None)
        .await
        .unwrap();
    assign(&engine, &session.id, &worker).await;
    let warm = service
        .warm_cache_now(&session.id, Some("worker"))
        .await
        .unwrap();
    assert!(warm.warmed);
    assert_eq!(warm.role, "worker");
    assert_eq!(warm.identity_id.as_deref(), Some(worker.as_str()));
    assert_eq!(warm.identity_generation, Some(1));
    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert!(
        head_section(&captured[0], "identity")
            .unwrap()
            .contains("Auth Tech Lead")
    );
    assert!(
        captured[0].tools.iter().any(|tool| tool.name == "delegate"),
        "Worker warm must include the delegation schema"
    );
    assert!(captured[0].tail_sections.is_empty());
    let report = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    let worker_state = report
        .states
        .iter()
        .find(|state| state.state_id != session.id)
        .expect("worker state");
    assert!(worker_state.identity_hash.is_some());
    assert!(
        report.latest_identity_id.is_none(),
        "warm emits no prompt prefix"
    );
}

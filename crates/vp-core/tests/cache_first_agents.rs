//! End-to-end cache-first prompt tests through the real agent loop and
//! provider adapters.
//!
//! The unit tests in `vp-providers` assert request bodies directly. These tests
//! additionally prove that project policy, prompt section assembly, the usage
//! ledger, and cache telemetry agree when a session actually runs.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{
    ApprovalMode, CachePolicy, ProjectCacheParams, RoleModels, SessionCacheCallsParams,
    SkillCreateParams,
};
use vp_providers::AnthropicProvider;
use vp_store::Store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "cache-first integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "cache-test@example.test"]);
    run(&["config", "user.name", "Cache Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

fn test_engine(dir: &TempDir) -> (Engine, String) {
    let store = Store::open(dir.path().join("data")).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("cache-project"))
        .unwrap();
    (engine, project.id)
}

fn service(engine: &Engine, provider: Arc<AnthropicProvider>, provider_name: &str) -> AgentService {
    let config = AgentConfig {
        provider,
        provider_name: provider_name.to_string(),
        model: "claude-test".to_string(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 3,
        max_tokens: Some(512),
        max_managers: 3,
        max_workers: 4,
        max_labour: 4,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 4,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(12, 24)),
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
    AgentService::configured(engine.clone(), config)
}

async fn wait_for_idle(engine: &Engine, session_id: &str) {
    for _ in 0..500 {
        let session = engine
            .store()
            .get_session(session_id)
            .unwrap()
            .expect("session exists");
        if matches!(
            session.status.as_str(),
            "idle" | "failed" | "cancelled" | "paused"
        ) {
            assert_ne!(
                session.status, "failed",
                "agent run failed: inspect agent.failed event"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("session {session_id} did not become idle");
}

async fn create_skill(engine: &Engine, project_id: &str) {
    engine
        .create_skill(SkillCreateParams {
            project_id: project_id.to_string(),
            name: "Cache Review".into(),
            description: "Review cache behavior.".into(),
            when_to_use: "When testing cache-first prompts.".into(),
            version: None,
            instructions: "Keep stable sections stable.".into(),
            tool_allow: Vec::new(),
            tool_deny: Vec::new(),
            profiles: Vec::new(),
            roles: vec!["leader".into()],
        })
        .await
        .unwrap();
}

fn count_cache_controls(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.contains_key("cache_control"))
                + object.values().map(count_cache_controls).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(count_cache_controls).sum(),
        _ => 0,
    }
}

fn anthropic_stream() -> String {
    concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":600,\"cache_creation_input_tokens\":200}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    )
    .to_string()
}

#[tokio::test]
async fn anthropic_agent_run_emits_four_breakpoints_and_cache_telemetry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_stream()),
        )
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let (engine, project_id) = test_engine(&dir);
    create_skill(&engine, &project_id).await;
    let provider = Arc::new(AnthropicProvider::with_base_url(
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("test-key".into()),
    ));
    let service = service(&engine, provider, "anthropic");
    let session = engine
        .create_session(project_id.clone(), "cache-first run".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "claude-test")
        .await
        .unwrap();
    engine
        .set_cache_policy(&project_id, CachePolicy::default())
        .await
        .unwrap();
    service.start(&session.id, "cache-first run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["cache_control"]["type"], "ephemeral");
    assert!(body["cache_control"].get("ttl").is_none());
    let system = body["system"].as_array().expect("system blocks");
    assert_eq!(system.len(), 3);
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(system[1]["cache_control"]["type"], "ephemeral");
    assert!(system[2].get("cache_control").is_none());
    assert_eq!(count_cache_controls(&body), 4);
    let tools = body["tools"].as_array().expect("tools");
    assert!(tools.last().unwrap().get("cache_control").is_some());

    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix event");
    assert_eq!(prefix.payload["cache_plan"]["provider"], "anthropic");
    assert_eq!(prefix.payload["cache_plan"]["breakpoints"], 4);
    assert_eq!(prefix.payload["cache_plan"]["automatic"], true);
    let cache = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache.payload["cache_breakpoints"], 4);
    assert_eq!(cache.payload["cache_ttl"], "5m");
    assert_eq!(cache.payload["cache_ttl_explicit"], true);
    assert_eq!(cache.payload["cache_read_tokens"], 600);
    assert_eq!(cache.payload["cache_write_tokens"], 200);
    assert_eq!(cache.payload["input_tokens"], 900);
    assert_eq!(cache.payload["miss_reason"], serde_json::Value::Null);
    assert_eq!(cache.payload["cache_miss_tokens"], 300);
    assert_eq!(cache.payload["theoretical_max_hit_ratio"], 0.0);
    assert!(cache.payload["message_prefix_hash"].as_str().is_some());
    assert_eq!(cache.payload["message_prefix_count"], 1);
    assert!(cache.payload["append_only"].is_null());
    assert!(cache.payload["cache_epoch"].as_str().is_some());
    assert!(cache.payload["cache_epoch_source"].as_str().is_some());
    assert!(cache.payload["core_hash"].as_str().is_some());
    assert!(cache.payload["project_hash"].as_str().is_some());
    assert!(cache.payload["tools_hash"].as_str().is_some());
    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.cache_read_tokens, 600);
    assert_eq!(status.cache_write_tokens, 200);
    assert_eq!(status.last_breakpoints, 4);
    assert_eq!(status.last_cache_ttl.as_deref(), Some("5m"));
    assert_eq!(status.recent_calls.len(), 1);
    assert_eq!(status.ttl_counts[0].key, "5m");
    assert_eq!(status.cache_miss_tokens, 300);
    assert_eq!(status.append_only_calls, 0);
    assert_eq!(status.append_only_breaks, 0);
    assert!(status.cache_epoch.is_some());

    let calls = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(1),
            role: Some("leader".into()),
            miss_reason: None,
            include_warm: false,
        })
        .await
        .unwrap();
    assert_eq!(calls.returned, 1);
    assert_eq!(calls.observed, 1);
    assert!(!calls.truncated);
    assert_eq!(calls.calls[0].cache_miss_tokens, 300);
    let hidden = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(1),
            role: Some("worker".into()),
            miss_reason: None,
            include_warm: false,
        })
        .await
        .unwrap();
    assert_eq!(hidden.returned, 0);
    assert_eq!(hidden.observed, 0);
}

fn anthropic_stream_with_usage(
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> String {
    format!(
        concat!(
            "event: message_start\n",
            "data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":{},\"cache_read_input_tokens\":{},\"cache_creation_input_tokens\":{}}}}}}}\n\n",
            "event: content_block_delta\n",
            "data: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"Done.\"}}}}\n\n",
            "event: message_delta\n",
            "data: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":3}}}}\n\n",
            "event: message_stop\n",
            "data: {{\"type\":\"message_stop\"}}\n\n",
        ),
        input_tokens, cache_read_tokens, cache_write_tokens
    )
}

#[tokio::test]
async fn second_agent_turn_proves_append_only_and_tail_delta_attribution() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_stream_with_usage(100, 0, 900)),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_stream_with_usage(200, 1_000, 0)),
        )
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let (engine, project_id) = test_engine(&dir);
    create_skill(&engine, &project_id).await;
    let provider = Arc::new(AnthropicProvider::with_base_url(
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("test-key".into()),
    ));
    let service = service(&engine, provider, "anthropic");
    let session = engine
        .create_session(project_id.clone(), "cache continuity".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .set_cache_policy(&project_id, CachePolicy::default())
        .await
        .unwrap();
    service.start(&session.id, "first turn").await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    service.start(&session.id, "second turn").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let cache_events = events
        .iter()
        .filter(|event| event.kind == "context.cache")
        .collect::<Vec<_>>();
    assert_eq!(cache_events.len(), 2);
    let first = cache_events[0];
    let second = cache_events[1];
    assert_eq!(first.payload["miss_reason"], "first_call");
    assert!(first.payload["append_only"].is_null());
    assert_eq!(second.payload["append_only"], true);
    assert_eq!(second.payload["miss_reason"], "tail_delta");
    assert_eq!(second.payload["cache_miss_tokens"], 200);
    assert_eq!(second.payload["previous_message_prefix_count"], 1);
    assert!(
        second.payload["message_prefix_count"].as_u64().unwrap()
            > second.payload["previous_message_prefix_count"]
                .as_u64()
                .unwrap()
    );
    assert_eq!(second.payload["cache_epoch"], first.payload["cache_epoch"]);
    assert_eq!(second.payload["cache_epoch_source"], "l4");

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.cache_miss_tokens, 1_200);
    assert_eq!(status.append_only_calls, 1);
    assert_eq!(status.append_only_breaks, 0);
    assert_eq!(status.last_append_only, Some(true));
    assert!(status.hit_ratio_p50.is_some());
    assert_eq!(status.top_miss_reasons[0].key, "first_call");
    assert_eq!(status.cache_epoch_timeline.len(), 1);

    let calls = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(20),
            role: None,
            miss_reason: None,
            include_warm: false,
        })
        .await
        .unwrap();
    assert_eq!(calls.returned, 2);
    assert_eq!(calls.calls[0].at, second.created_at);
    assert_eq!(calls.calls[0].append_only, Some(true));
    assert_eq!(calls.calls[1].append_only, None);

    let tail_only = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(1),
            role: None,
            miss_reason: Some("tail_delta".into()),
            include_warm: false,
        })
        .await
        .unwrap();
    assert_eq!(tail_only.returned, 1);
    assert_eq!(tail_only.observed, 1);
    assert!(!tail_only.truncated);
    assert_eq!(
        tail_only.calls[0].miss_reason.as_deref(),
        Some("tail_delta")
    );

    let limited = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(1),
            role: None,
            miss_reason: None,
            include_warm: false,
        })
        .await
        .unwrap();
    assert_eq!(limited.returned, 1);
    assert_eq!(limited.observed, 2);
    assert!(limited.truncated);

    let overview = engine
        .project_cache_overview(&ProjectCacheParams {
            project_id: project_id.clone(),
            limit: Some(1_000),
        })
        .await
        .unwrap();
    assert_eq!(overview.sessions_observed, 1);
    assert_eq!(overview.calls, 2);
    assert_eq!(overview.input_tokens, 2_200);
    assert_eq!(overview.cache_miss_tokens, 1_200);
    assert_eq!(overview.append_only_calls, 1);
    assert_eq!(overview.append_only_breaks, 0);
    assert_eq!(overview.cache_epochs, 1);
    assert!(!overview.truncated);
    assert_eq!(overview.scan_limit, 1_000);

    engine
        .store()
        .append_event(
            &session.id,
            "context.cache",
            serde_json::json!({
                "warm": true,
                "input_tokens": 500,
                "cache_read_tokens": 400,
                "output_tokens": 5,
                "cache_epoch": "epoch-warm",
                "agent_role": "cache_warm"
            }),
        )
        .unwrap();
    let without_warm = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(20),
            role: None,
            miss_reason: None,
            include_warm: false,
        })
        .await
        .unwrap();
    assert_eq!(without_warm.returned, 2);
    let with_warm = engine
        .session_cache_calls(&SessionCacheCallsParams {
            session_id: session.id.clone(),
            limit: Some(20),
            role: Some("cache_warm".into()),
            miss_reason: None,
            include_warm: true,
        })
        .await
        .unwrap();
    assert_eq!(with_warm.returned, 1);
    assert!(with_warm.calls[0].warm);
}

#[tokio::test]
async fn anthropic_one_hour_policy_reaches_the_wire_and_telemetry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(anthropic_stream()),
        )
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let (engine, project_id) = test_engine(&dir);
    create_skill(&engine, &project_id).await;
    let provider = Arc::new(AnthropicProvider::with_base_url(
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("test-key".into()),
    ));
    let service = service(&engine, provider, "anthropic");
    engine
        .set_cache_policy(
            &project_id,
            CachePolicy {
                ttl: "1h".into(),
                ..CachePolicy::default()
            },
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project_id, "one hour cache".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "claude-test")
        .await
        .unwrap();
    service.start(&session.id, "one hour cache").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["cache_control"]["ttl"], "1h");
    let system = body["system"].as_array().unwrap();
    assert_eq!(system[0]["cache_control"]["ttl"], "1h");
    assert_eq!(system[1]["cache_control"]["ttl"], "1h");
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let cache = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache.payload["cache_ttl"], "1h");
    assert_eq!(cache.payload["cache_breakpoints"], 4);
}

#[tokio::test]
async fn openai_compatible_agent_keeps_stable_sections_first_and_sorted_tools() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"}}]}\n\n",
                    "data: {\"usage\":{\"prompt_tokens\":1000,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .mount(&server)
        .await;
    let dir = TempDir::new().unwrap();
    let (engine, project_id) = test_engine(&dir);
    create_skill(&engine, &project_id).await;
    let provider = Arc::new(vp_providers::OpenAiCompatibleProvider::new(
        "openai-compatible",
        format!("{}/v1", server.uri()),
        Some("test-key".into()),
    ));
    let config = AgentConfig {
        provider,
        provider_name: "openai-compatible".into(),
        model: "gpt-test".into(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 3,
        max_tokens: Some(512),
        max_managers: 3,
        max_workers: 4,
        max_labour: 4,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 4,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(12, 24)),
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
    let session = engine
        .create_session(project_id, "openai cache".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "gpt-test")
        .await
        .unwrap();
    service.start(&session.id, "openai cache").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Role charter:")
    );
    assert_eq!(messages[1]["role"], "system");
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("Cache Review")
    );
    assert_eq!(messages[2]["role"], "system");
    assert!(
        messages[2]["content"]
            .as_str()
            .unwrap()
            .contains("Session goal:")
    );
    assert_eq!(messages[3]["role"], "user");
    assert!(body.get("cache_control").is_none());
    assert!(!body.to_string().contains("cache_control"));
    let names = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);

    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix");
    assert_eq!(
        prefix.payload["cache_plan"]["provider"],
        "openai-compatible"
    );
    assert_eq!(prefix.payload["cache_plan"]["breakpoints"], 0);
    assert_eq!(prefix.payload["cache_plan"]["automatic"], true);
    let cache = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache.payload["cache_breakpoints"], 0);
    assert_eq!(cache.payload["cache_ttl_explicit"], false);
    assert_eq!(cache.payload["miss_reason"], "first_call");
}

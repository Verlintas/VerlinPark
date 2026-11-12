//! End-to-end append-only conversation tests through the real agent loop.
//!
//! The provider unit tests assert raw HTTP bodies. These tests prove that the
//! runtime records the same canonical identity, that normal turns append, that
//! memory is anchored once instead of moving, and that every provider call has
//! canonical telemetry. No test edits a prior message to game a cache hit.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{ApprovalMode, RoleModels};
use vp_providers::{
    ChatMessage, ChatRequest, EventStream, Provider, ProviderCacheCapabilities, ProviderError,
    StreamEvent, ToolCall, canonical_request,
};
use vp_store::Store;

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "append-only integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "append-test@example.test"]);
    run(&["config", "user.name", "Append Test"]);
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "initial"]);
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn cache_capabilities(&self) -> ProviderCacheCapabilities {
        ProviderCacheCapabilities::openai_compatible()
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done]);
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

/// Same recording behavior with an explicit cache capability. Used to prove
/// that a provider downgrade is recorded and reflected in request assembly.
struct CapabilityProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
    capabilities: ProviderCacheCapabilities,
}

#[async_trait]
impl Provider for CapabilityProvider {
    fn id(&self) -> &str {
        "capability"
    }

    fn cache_capabilities(&self) -> ProviderCacheCapabilities {
        self.capabilities
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done]);
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

fn test_engine(dir: &TempDir) -> (Engine, String) {
    let store = Store::open(dir.path().join("data")).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("append-project"))
        .unwrap();
    (engine, project.id)
}

fn service(engine: &Engine, provider: Arc<RecordingProvider>) -> AgentService {
    let config = AgentConfig {
        provider,
        provider_name: "openai-compatible".to_string(),
        model: "test-model".to_string(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 6,
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
    for _ in 0..600 {
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

fn message_value(message: &ChatMessage) -> Value {
    serde_json::to_value(message).unwrap()
}

fn canonical_history(request: &ChatRequest) -> Vec<Value> {
    canonical_request(
        request,
        ProviderCacheCapabilities::openai_compatible(),
        "tools",
    )
    .history_messages
}

fn assert_value_prefix(previous: &[Value], current: &[Value]) {
    assert!(current.len() >= previous.len());
    for (index, message) in previous.iter().enumerate() {
        assert_eq!(&current[index], message, "prior message {index} changed");
    }
}

#[tokio::test]
async fn normal_turns_are_append_only_and_carry_canonical_telemetry() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_todo".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [{"title": "inspect", "status": "in_progress"}]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("finished".into()), StreamEvent::Done],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "append-only normal turns".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "inspect the repo")
        .await
        .unwrap();
    wait_for_idle(&engine, &session.id).await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2, "one provider call per turn");
    let first = canonical_history(&captured[0]);
    let second = canonical_history(&captured[1]);
    assert_value_prefix(&first, &second);
    assert_eq!(first[0]["content"], serde_json::json!("inspect the repo"));

    let events = engine
        .recent_events_of_kind(&session.id, "context.cache", 20)
        .await
        .unwrap();
    assert_eq!(events.len(), 2, "two ordinary cache observations");
    let first_event = &events[0].payload;
    let second_event = &events[1].payload;
    assert!(first_event["append_only"].is_null());
    assert_eq!(first_event["cache_break_reason"], "first_call");
    assert_eq!(second_event["append_only"], true);
    assert_eq!(
        second_event["previous_message_prefix_hash"],
        first_event["message_prefix_hash"]
    );
    assert_eq!(first_event["message_count"], 1);
    assert_eq!(second_event["message_count"], 3);
    assert_eq!(first_event["canonical_version"], 1);
    assert_eq!(first_event["provider_wire"], "openai-compatible");
    assert_eq!(first_event["tail_memory_mode"], "none");
    assert_eq!(first_event["provider_tail_system"], true);
    assert_eq!(
        first_event["tail_system_capability"],
        serde_json::json!("supported")
    );
    assert!(first_event["message_prefix_hash"].as_str().unwrap().len() == 64);
    // The second call has no provider-reported usage in this fake provider, so
    // the runtime records an honest provider-side miss on an append-only list.
    assert_eq!(
        second_event["cache_break_reason"],
        serde_json::json!("provider_cache_miss")
    );

    let breaks = engine
        .recent_events_of_kind(&session.id, "context.cache_break", 20)
        .await
        .unwrap();
    let reasons = breaks
        .iter()
        .map(|event| event.payload["reason"].as_str().unwrap_or(""))
        .collect::<Vec<_>>();
    assert!(reasons.contains(&"first_call"));
    assert!(reasons.contains(&"provider_cache_miss"));
    for event in &breaks {
        assert!(!event.payload.to_string().contains("inspect the repo"));
        assert!(!event.payload.to_string().contains("finished"));
    }
}

#[tokio::test]
async fn memory_is_appended_once_and_keeps_history_append_only_across_runs() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("first answer".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second answer".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("third answer".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "append-only memory".into(), None)
        .await
        .unwrap();
    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "First memory",
            "The release registry is internal.",
            vec!["release".into()],
        )
        .await
        .unwrap();
    service.start(&session.id, "first run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_value_prefix(
            &canonical_history(&captured[0]),
            &canonical_history(&captured[0]),
        );
        assert!(captured[0].messages.iter().any(
            |message| message.role == "system" && message.content.contains("release registry")
        ));
    }
    let anchors_after_first = engine
        .list_messages(&session.id)
        .await
        .unwrap()
        .iter()
        .filter(|message| message.role == "system")
        .count();
    assert_eq!(anchors_after_first, 1);

    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "Second memory",
            "The build uses the workspace cache.",
            vec!["build".into()],
        )
        .await
        .unwrap();
    service.start(&session.id, "second run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        let first = canonical_history(&captured[0]);
        let second = canonical_history(&captured[1]);
        // The first run's full request is an exact prefix of the second run's
        // first request. The changed memory was appended after the new prompt,
        // not reinserted ahead of prior messages.
        assert_value_prefix(&first, &second);
        assert!(second.len() > first.len());
        let second_anchor = second
            .iter()
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("workspace cache"))
            })
            .count();
        assert_eq!(second_anchor, 1);
        let first_anchor = second
            .iter()
            .filter(|message| {
                message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("release registry"))
            })
            .count();
        // The newer brief can include the older fact as well; what matters is
        // that the first anchor is still present at its original index (proven
        // by the prefix assertion above) and that no anchor was rewritten.
        assert!(first_anchor >= 1);
    }
    let anchors_after_second = engine
        .list_messages(&session.id)
        .await
        .unwrap()
        .iter()
        .filter(|message| message.role == "system")
        .count();
    assert_eq!(anchors_after_second, 2);

    // A third run with unchanged memory must not append a duplicate anchor.
    service.start(&session.id, "third run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    let anchors_after_third = engine
        .list_messages(&session.id)
        .await
        .unwrap()
        .iter()
        .filter(|message| message.role == "system")
        .count();
    assert_eq!(anchors_after_third, 2);

    let cache = engine
        .recent_events_of_kind(&session.id, "context.cache", 20)
        .await
        .unwrap();
    assert_eq!(cache.len(), 3);
    assert!(cache[0].payload["append_only"].is_null());
    assert_eq!(cache[1].payload["append_only"], true);
    assert_eq!(cache[2].payload["append_only"], true);
    assert_eq!(
        cache[1].payload["previous_message_prefix_hash"],
        cache[0].payload["message_prefix_hash"]
    );
    assert_eq!(cache[1].payload["tail_memory_mode"], "embedded");
    assert_eq!(cache[1].payload["message_count"], 5);

    let anchor_events = engine
        .recent_events_of_kind(&session.id, "context.memory.anchor", 20)
        .await
        .unwrap();
    let appended = anchor_events
        .iter()
        .filter(|event| event.payload["appended"] == true)
        .count();
    let reused = anchor_events
        .iter()
        .filter(|event| event.payload["appended"] == false)
        .count();
    assert_eq!(appended, 2);
    assert!(reused >= 1);
    for event in &anchor_events {
        assert!(!event.payload.to_string().contains("release registry"));
        assert!(!event.payload.to_string().contains("workspace cache"));
    }
}

#[tokio::test]
async fn first_call_break_has_no_previous_hash_and_legacy_events_still_decode() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "first call".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "hello").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let cache = engine
        .recent_events_of_kind(&session.id, "context.cache", 5)
        .await
        .unwrap();
    assert_eq!(cache.len(), 1);
    assert!(cache[0].payload["previous_message_prefix_hash"].is_null());
    assert_eq!(cache[0].payload["previous_message_count"], 0);

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert!(status.cache_calls >= 1);
    assert!(!status.last_append_only.unwrap_or(false));
    assert!(
        status
            .recent_breaks
            .iter()
            .any(|entry| entry.reason == "first_call")
    );
    assert!(status.last_message_prefix_hash.is_some());
    assert_eq!(status.append_only_calls, 0);
    assert_eq!(status.non_append_only_calls, 0);
    assert_eq!(
        status.last_tail_system_capability.as_deref(),
        Some("supported")
    );
    assert_eq!(status.provider_tail_system, Some(true));
}

#[tokio::test]
async fn session_cache_projection_is_bounded_and_content_free() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [{"title": "task", "status": "completed"}]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("all done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "bounded projection".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "do work").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.append_only_calls, 1);
    assert_eq!(status.non_append_only_calls, 0);
    assert!(status.recent_calls.len() <= 20);
    assert!(status.recent_breaks.len() <= 20);
    assert!(
        status
            .cache_break_counts
            .iter()
            .any(|bucket| bucket.key == "first_call")
    );
    assert_eq!(
        status.recent_calls[0].message_prefix_hash.as_deref(),
        status.last_message_prefix_hash.as_deref()
    );
    assert_eq!(status.recent_calls[0].append_only, Some(true));
    let encoded = serde_json::to_string(&status).unwrap();
    assert!(!encoded.contains("do work"));
    assert!(!encoded.contains("all done"));
    assert!(!encoded.contains("completed"));
}

#[tokio::test]
async fn tool_result_append_keeps_the_previous_messages_exact() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_todo".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [{"title": "work", "status": "in_progress"}]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "append after tool call".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "first prompt").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let first = canonical_history(&captured[0]);
    let second = canonical_history(&captured[1]);
    assert_value_prefix(&first, &second);
    // The tool result was appended and the original prompt bytes are intact.
    assert_eq!(
        message_value(&captured[0].messages[0]),
        message_value(&captured[1].messages[0])
    );
}

#[tokio::test]
async fn compaction_between_runs_is_reported_as_a_compaction_reset() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![StreamEvent::TextDelta("one".into()), StreamEvent::Done],
            vec![StreamEvent::TextDelta("two".into()), StreamEvent::Done],
            vec![
                StreamEvent::TextDelta("after compaction".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "compaction reset".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "first prompt").await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    service.start(&session.id, "second prompt").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let messages = engine.list_messages(&session.id).await.unwrap();
    let boundary = messages
        .iter()
        .find(|message| message.content == "first prompt")
        .expect("first prompt stored");
    // A compaction boundary strictly after the last provider call. The next
    // run must attribute the discontinuity to compaction, not to an edit or a
    // provider eviction.
    engine
        .append_event(
            &session.id,
            "context.compacted",
            serde_json::json!({
                "mode": "summary",
                "through_message_id": boundary.id,
                "summary": "Earlier conversation summary (test):\nfirst prompt",
                "summarized_messages": 1,
                "kept_messages": 3
            }),
        )
        .await
        .unwrap();

    service.start(&session.id, "third prompt").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let cache = engine
        .recent_events_of_kind(&session.id, "context.cache", 10)
        .await
        .unwrap();
    let latest = cache.last().unwrap();
    assert_eq!(latest.payload["append_only"], false);
    assert_eq!(
        latest.payload["cache_break_reason"],
        serde_json::json!("compaction_reset")
    );
    let breaks = engine
        .recent_events_of_kind(&session.id, "context.cache_break", 10)
        .await
        .unwrap();
    assert!(
        breaks
            .iter()
            .any(|event| event.payload["reason"] == "compaction_reset")
    );
    assert_eq!(requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn cache_breaks_rpc_is_bounded_and_newest_first() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [{"title": "work", "status": "in_progress"}]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "break rpc".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "do work").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let result = engine
        .session_cache_breaks(&session.id, Some(1))
        .await
        .unwrap();
    assert_eq!(result.session_id, session.id);
    assert_eq!(result.append_only_calls, 1);
    assert_eq!(result.non_append_only_calls, 0);
    assert_eq!(result.breaks.len(), 1);
    assert!(result.total_breaks >= 2);
    assert!(result.truncated);
    // Newest first: the provider miss is post-call and later than first_call.
    assert_eq!(result.breaks[0].reason, "provider_cache_miss");
    assert_eq!(result.breaks[0].message_count, 3);

    let all = engine
        .session_cache_breaks(&session.id, Some(200))
        .await
        .unwrap();
    assert!(!all.truncated);
    assert!(all.breaks.iter().any(|entry| entry.reason == "first_call"));
    let encoded = serde_json::to_string(&all).unwrap();
    assert!(!encoded.contains("do work"));
    assert!(!encoded.contains("done"));
}

#[test]
fn provider_canonical_tools_hash_matches_the_tool_bundle_contract() {
    use vp_providers::ToolSchema;
    let tools = vec![
        ToolSchema {
            name: "write_file".into(),
            description: "Write a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}}
            }),
        },
        ToolSchema {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    ];
    assert_eq!(
        vp_providers::canonical_tools_hash(&tools),
        vp_core::tool_bundle::canonical_tools_hash(&tools)
    );
    // Order does not matter for either contract.
    let mut reversed = tools;
    reversed.reverse();
    assert_eq!(
        vp_providers::canonical_tools_hash(&reversed),
        vp_core::tool_bundle::canonical_tools_hash(&reversed)
    );
}

#[tokio::test]
async fn memory_anchor_survives_a_daemon_restart_without_a_duplicate() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("before restart".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("after restart".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let first_service = service(&engine, provider.clone());
    let session = engine
        .create_session(project_id, "restart anchor".into(), None)
        .await
        .unwrap();
    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "Restart memory",
            "The restart anchor must not duplicate.",
            vec!["restart".into()],
        )
        .await
        .unwrap();
    first_service.start(&session.id, "first run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    let first_cache = engine
        .recent_events_of_kind(&session.id, "context.cache", 10)
        .await
        .unwrap();
    assert_eq!(first_cache.len(), 1);
    drop(first_service);
    drop(engine);

    // Reopen the same data directory. This is the same persisted state a
    // daemon restart sees: no in-memory tracker, no in-memory conversation.
    let store = Store::open(dir.path().join("data")).unwrap();
    let restarted = Engine::new(store);
    let project = restarted
        .store()
        .list_projects()
        .unwrap()
        .into_iter()
        .next()
        .expect("project persists");
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("after restart".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second follow-up".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let service = service(&restarted, provider);
    service.start(&session.id, "second run").await.unwrap();
    wait_for_idle(&restarted, &session.id).await;

    let anchors = restarted
        .list_messages(&session.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|message| message.role == "system")
        .count();
    assert_eq!(
        anchors, 1,
        "unchanged memory must reuse the persisted anchor"
    );
    let cache = restarted
        .recent_events_of_kind(&session.id, "context.cache", 10)
        .await
        .unwrap();
    assert_eq!(cache.len(), 2);
    assert_eq!(cache[1].payload["append_only"], true);
    assert_eq!(
        cache[1].payload["previous_message_prefix_hash"],
        cache[0].payload["message_prefix_hash"]
    );
    assert_eq!(cache[1].payload["tail_memory_mode"], "embedded");
    assert_eq!(project.id, session.project_id);

    service.start(&session.id, "third run").await.unwrap();
    wait_for_idle(&restarted, &session.id).await;
    let anchors = restarted
        .list_messages(&session.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|message| message.role == "system")
        .count();
    assert_eq!(anchors, 1);
}

#[tokio::test]
async fn unsupported_tail_system_is_recorded_and_uses_the_user_projection() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(CapabilityProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Done,
        ]])),
        capabilities: ProviderCacheCapabilities::new(
            vp_providers::ProviderWire::OpenAiCompatible,
            vp_providers::TailSystemCapability::Unsupported,
        ),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service_with_capabilities(&engine, provider);
    let session = engine
        .create_session(project_id, "capability fallback".into(), None)
        .await
        .unwrap();
    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "Capability memory",
            "Use the stable user fallback for memory.",
            vec!["memory".into()],
        )
        .await
        .unwrap();
    service.start(&session.id, "run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let cache = engine
        .recent_events_of_kind(&session.id, "context.cache", 5)
        .await
        .unwrap();
    let payload = &cache[0].payload;
    assert_eq!(payload["tail_system_capability"], "unsupported");
    assert_eq!(payload["provider_tail_system"], false);
    assert_eq!(payload["tail_memory_mode"], "embedded");

    let captured = requests.lock().unwrap().clone();
    let history = canonical_request(
        &captured[0],
        ProviderCacheCapabilities::new(
            vp_providers::ProviderWire::OpenAiCompatible,
            vp_providers::TailSystemCapability::Unsupported,
        ),
        "tools",
    )
    .history_messages;
    let anchor = history
        .iter()
        .find(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("stable user fallback"))
        })
        .expect("memory anchor is present");
    assert_eq!(anchor["role"], serde_json::json!("user"));
    assert!(
        anchor["content"]
            .as_str()
            .unwrap()
            .starts_with("[memory]\n\n")
    );
}

#[tokio::test]
async fn provider_capability_change_is_recorded_once() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(CapabilityProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Done,
        ]])),
        capabilities: ProviderCacheCapabilities::new(
            vp_providers::ProviderWire::OpenAiCompatible,
            vp_providers::TailSystemCapability::Unsupported,
        ),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service_with_capabilities(&engine, provider);
    let session = engine
        .create_session(project_id, "capability event".into(), None)
        .await
        .unwrap();
    // Seed a previous supported observation with no canonical message hash, so
    // the run records first_call and a capability change.
    engine
        .append_event(
            &session.id,
            "context.cache",
            serde_json::json!({
                "agent_role": "leader",
                "provider_wire": "openai-compatible",
                "tail_system_capability": "supported",
                "provider_tail_system": true,
                "warm": false
            }),
        )
        .await
        .unwrap();
    service.start(&session.id, "run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let events = engine
        .recent_events_of_kind(&session.id, "context.cache_capability", 5)
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["provider_wire"], "openai-compatible");
    assert_eq!(events[0].payload["tail_system_capability"], "unsupported");
    assert_eq!(
        events[0].payload["previous_tail_system_capability"],
        "supported"
    );
    assert_eq!(events[0].payload["provider_tail_system"], false);
    assert_eq!(events[0].payload["provider"], "openai-compatible");
}

#[tokio::test]
async fn prompt_prefix_records_the_memory_anchor_identity_without_text() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "anchor identity".into(), None)
        .await
        .unwrap();
    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "Anchor identity",
            "Do not print this memory text in telemetry.",
            vec!["telemetry".into()],
        )
        .await
        .unwrap();
    service.start(&session.id, "run").await.unwrap();
    wait_for_idle(&engine, &session.id).await;
    let events = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 5)
        .await
        .unwrap();
    let payload = &events[0].payload;
    assert_eq!(payload["canonical_version"], 1);
    assert_eq!(payload["provider_wire"], "openai-compatible");
    assert_eq!(payload["tail_system_capability"], "supported");
    assert_eq!(payload["provider_tail_system"], true);
    assert_eq!(payload["memory_anchor"]["appended"], true);
    assert_eq!(
        payload["memory_anchor"]["anchor_hash"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert!(
        !payload
            .to_string()
            .contains("Do not print this memory text")
    );
    assert!(!payload.to_string().contains("telemetry"));
}

/// Build a service whose provider is not the default recording provider.
fn service_with_capabilities(engine: &Engine, provider: Arc<CapabilityProvider>) -> AgentService {
    let config = AgentConfig {
        provider,
        provider_name: "openai-compatible".to_string(),
        model: "test-model".to_string(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 6,
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

#[tokio::test]
async fn cache_audit_distinguishes_provider_miss_from_local_churn() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [{"title": "work", "status": "in_progress"}]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "audit".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "do work").await.unwrap();
    wait_for_idle(&engine, &session.id).await;

    let audit = engine.session_cache_audit(&session.id).await.unwrap();
    let codes = audit
        .findings
        .iter()
        .map(|finding| finding.code.as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"provider_evicted_append_only_prefix"),
        "codes: {codes:?}"
    );
    assert!(!codes.contains(&"non_append_only_history"));
    assert!(!codes.contains(&"legacy_events_without_continuity"));
    let finding = audit
        .findings
        .iter()
        .find(|finding| finding.code == "provider_evicted_append_only_prefix")
        .unwrap();
    assert_eq!(finding.severity, "info");
    assert_eq!(finding.evidence["append_only_calls"], 1);
    assert!(!audit.findings.iter().any(|finding| {
        finding.message.contains("do work") || finding.message.contains("done")
    }));
}

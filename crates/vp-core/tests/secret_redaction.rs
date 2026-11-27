//! S1 regression tests for deterministic secret redaction at the persistence
//! boundary. The real tool input/output must still be correct; only the copies
//! stored in tool events, approval rows, and approval events are sanitized.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::permissions::{ApprovalDecision, PermissionAssessment, RiskLevel};
use vp_core::tooling::redaction::{redact_persisted_text, redact_persisted_value};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{ApprovalMode, RoleModels};
use vp_providers::{
    ChatRequest, EventStream, Provider, ProviderCacheCapabilities, ProviderError, StreamEvent,
    ToolCall,
};
use vp_store::Store;

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "secret redaction integration\n").unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "redaction-test@example.test"]);
    run(&["config", "user.name", "Redaction Test"]);
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
        .add_project(repo.to_str().unwrap(), Some("redaction-project"))
        .unwrap();
    (engine, project.id)
}

struct ScriptProvider {
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
}

#[async_trait]
impl Provider for ScriptProvider {
    fn id(&self) -> &str {
        "redaction-script"
    }

    fn cache_capabilities(&self) -> ProviderCacheCapabilities {
        ProviderCacheCapabilities::openai_compatible()
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
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

fn service(engine: &Engine, provider: Arc<ScriptProvider>) -> AgentService {
    let config = AgentConfig {
        provider,
        provider_name: "openai-compatible".to_string(),
        model: "test-model".to_string(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: Some(4),
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
        approval_mode: ApprovalMode::Smart,
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

const SECRETS: [&str; 4] = [
    "sk_live_abcdefgh1234",
    "topsecret",
    "user:pass",
    "querysecret",
];

fn assert_payload_redacted(payload: &serde_json::Value) {
    let text = payload.to_string();
    for secret in SECRETS {
        assert!(
            !text.contains(secret),
            "persisted payload leaked {secret}: {text}"
        );
    }
    assert!(
        text.contains("[redacted]"),
        "persisted payload was not redacted: {text}"
    );
}

#[tokio::test]
async fn tool_events_are_redacted_but_the_real_result_is_unchanged() {
    let dir = TempDir::new().unwrap();
    let command = concat!(
        "printf '%s\\n' ",
        "'API_KEY=sk_live_abcdefgh1234' ",
        "'Cookie: session=topsecret; theme=dark' ",
        "'https://user:pass@example.test/x?token=querysecret&keep=1'"
    );
    let provider = Arc::new(ScriptProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_secret".into(),
                    name: "shell".into(),
                    arguments: json!({"command": command}).to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, project_id) = test_engine(&dir);
    let service = service(&engine, provider);
    let session = engine
        .create_session(project_id, "redact tool persistence".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "exercise the shell tool")
        .await
        .unwrap();
    wait_for_idle(&engine, &session.id).await;

    let events = engine.all_events(&session.id).await.unwrap();
    let started = events
        .iter()
        .find(|event| {
            event.kind == "tool.started" && event.payload["tool_call_id"] == "call_secret"
        })
        .expect("tool.started event");
    assert_payload_redacted(&started.payload);
    let started_command = started.payload["input"]["command"]
        .as_str()
        .expect("shell command input");
    assert!(started_command.contains("[redacted]"));
    assert_eq!(started.payload["name"], json!("shell"));

    let finished = events
        .iter()
        .find(|event| {
            event.kind == "tool.finished" && event.payload["tool_call_id"] == "call_secret"
        })
        .expect("tool.finished event");
    assert_payload_redacted(&finished.payload);
    let output = finished.payload["output"]
        .as_str()
        .expect("shell output text");
    assert!(output.contains("[redacted]"));
    assert!(
        output.contains("keep=1"),
        "redaction must preserve unrelated query parameters: {output}"
    );
    assert_eq!(finished.payload["is_error"], json!(false));

    // The tool itself and the provider-facing tool result are unchanged.
    let messages = engine.list_messages(&session.id).await.unwrap();
    let tool_message = messages
        .iter()
        .find(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("call_secret")
        })
        .expect("stored tool result message");
    assert!(
        tool_message
            .content
            .contains("API_KEY=sk_live_abcdefgh1234")
    );
    assert!(tool_message.content.contains("session=topsecret"));
    assert!(tool_message.content.contains("user:pass"));
}

#[tokio::test]
async fn approval_rows_and_events_are_redacted_while_evaluation_uses_the_real_input() {
    let dir = TempDir::new().unwrap();
    let (engine, project_id) = test_engine(&dir);
    let session = engine
        .create_session(project_id.clone(), "redact approval".into(), None)
        .await
        .unwrap();
    let input = json!({
        "command": concat!(
            "curl -H 'Authorization: Bearer sk_live_abcdefgh1234' ",
            "'https://user:pass@example.test/x?token=querysecret&keep=1'"
        )
    });
    let assessment = PermissionAssessment::new(RiskLevel::HostState, "host-state test");

    // Evaluation still sees the raw value (it runs before the store write).
    let evaluation = engine
        .evaluate_tool_permission(
            &session.id,
            &project_id,
            &assessment,
            "shell",
            &input,
            ApprovalMode::Smart,
            false,
        )
        .unwrap();
    assert!(matches!(evaluation.decision, ApprovalDecision::Ask { .. }));
    assert!(input.to_string().contains("sk_live_abcdefgh1234"));

    let (approval, _receiver) = engine
        .request_approval_with_risk(
            &session.id,
            "call_approval",
            "shell",
            input.clone(),
            Some(json!({"agent_id": "leader-1", "agent_name": "Leader"})),
            Some("host_state"),
            Some("host-state test"),
        )
        .await
        .unwrap();
    assert_payload_redacted(&approval.input);
    assert!(approval.input.to_string().contains("keep=1"));

    let stored = engine
        .store()
        .get_approval(&approval.id)
        .unwrap()
        .expect("approval row");
    assert_payload_redacted(&stored.input);

    let events = engine.all_events(&session.id).await.unwrap();
    for kind in ["approval.created", "approval.requested"] {
        let event = events
            .iter()
            .find(|event| {
                event.kind == kind && event.payload["id"].as_str() == Some(approval.id.as_str())
            })
            .unwrap_or_else(|| panic!("{kind} event"));
        assert_payload_redacted(&event.payload["input"]);
    }

    // Read-side defense: a row written directly by an older build is hidden
    // from list/queue projections even though the SQLite row is unchanged.
    let legacy = engine
        .store()
        .create_approval(
            &session.id,
            "legacy_call",
            "shell",
            &input,
            Some("host_state"),
            Some("legacy row"),
        )
        .unwrap();
    let listed = engine
        .list_approvals(Some(session.id.clone()))
        .await
        .unwrap();
    let legacy_view = listed
        .iter()
        .find(|row| row.id == legacy.id)
        .expect("legacy approval in list");
    assert_payload_redacted(&legacy_view.input);

    engine.resolve_approval(&approval.id, false).await.unwrap();
}

#[test]
fn redaction_is_idempotent_and_leaves_ordinary_code_alone() {
    let ordinary = concat!(
        "let token_count = 3;\n",
        "fn password_policy() -> bool { true }\n",
        "// credentials are checked by the auth module; no literal here\n",
        "assert_eq!(max_tokens, 512);\n"
    );
    assert_eq!(redact_persisted_text(ordinary), ordinary);

    let text = concat!(
        "API_KEY=sk_live_abcdefgh1234\n",
        "token: abc123\n",
        "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature123\n",
        "Cookie: session=topsecret; theme=dark\n",
        "https://user:pass@example.test/x?token=querysecret&keep=1"
    );
    let once = redact_persisted_text(text);
    assert_eq!(once, redact_persisted_text(&once));

    let value = json!({
        "headers": {"Authorization": "Bearer sk_live_abcdefgh1234", "Accept": "application/json"},
        "query": "https://example.test/x?key=querysecret&keep=1",
        "label": "ordinary text",
    });
    let once = redact_persisted_value(&value);
    assert_eq!(once, redact_persisted_value(&once));
    assert_eq!(once["headers"]["Accept"], json!("application/json"));
    assert_eq!(once["label"], json!("ordinary text"));
    assert!(once.to_string().contains("keep=1"));
    assert!(!once.to_string().contains("querysecret"));
}

use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use vp_providers::{EventStream, ProviderError};
use vp_store::Store;

struct QueueProvider {
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
}

#[async_trait]
impl Provider for QueueProvider {
    fn id(&self) -> &str {
        "queue"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::TextDelta("done".into())]);
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

fn test_service(dir: &TempDir) -> (Engine, AgentService, vp_protocol::types::Project) {
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("I read the README.".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    test_service_with_provider(dir, provider)
}

fn test_service_with_provider(
    dir: &TempDir,
    provider: Arc<dyn Provider>,
) -> (Engine, AgentService, vp_protocol::types::Project) {
    test_service_with_provider_cap(dir, provider, 4)
}

fn test_service_with_provider_cap(
    dir: &TempDir,
    provider: Arc<dyn Provider>,
    max_active_sessions: usize,
) -> (Engine, AgentService, vp_protocol::types::Project) {
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let project_dir = dir.path().join("repo");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("README.md"), "hello from test\n").unwrap();
    init_repo(&project_dir);
    let project = engine
        .store()
        .add_project(project_dir.to_str().unwrap(), Some("repo"))
        .unwrap();
    let config = AgentConfig {
        provider,
        provider_name: "queue".into(),
        model: "test".into(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 4,
        max_tokens: Some(1024),
        max_managers: 5,
        max_workers: 10,
        max_labour: 20,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(
            global_worker_cap(),
            global_labour_cap(),
        )),
        provider_idle_timeout_secs: 300,
        question_timeout_secs: 1800,
        approval_mode: ApprovalMode::default(),
        compact_after_tokens: 0,
        compact_keep_messages: 8,
        context_window: 0,
    };
    let service = AgentService::configured(engine.clone(), config);
    (engine, service, project)
}

fn set_approval_mode(service: &AgentService, mode: ApprovalMode) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.approval_mode = mode;
    *service.config.write().unwrap() = Some(config);
}

fn set_worker_cap(service: &AgentService, max_workers: usize) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.max_workers = max_workers;
    *service.config.write().unwrap() = Some(config);
}

fn set_compaction(service: &AgentService, after_tokens: u64, keep_messages: usize) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.compact_after_tokens = after_tokens;
    config.compact_keep_messages = keep_messages;
    config.context_window = 8_000;
    *service.config.write().unwrap() = Some(config);
}

fn set_provider_idle_timeout(service: &AgentService, seconds: u64) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.provider_idle_timeout_secs = seconds;
    *service.config.write().unwrap() = Some(config);
}

fn set_integration_command(service: &AgentService, command: Option<&str>) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.integration_command = command.map(ToOwned::to_owned);
    *service.config.write().unwrap() = Some(config);
}

struct SlowProvider;

#[async_trait]
impl Provider for SlowProvider {
    fn id(&self) -> &str {
        "slow"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(StreamEvent::TextDelta("late".into()))
        })))
    }
}

struct ScriptedStreamProvider {
    responses: Mutex<VecDeque<Vec<Result<StreamEvent, ProviderError>>>>,
}

#[async_trait]
impl Provider for ScriptedStreamProvider {
    fn id(&self) -> &str {
        "scripted-stream"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                vec![
                    Ok(StreamEvent::TextDelta("done".into())),
                    Ok(StreamEvent::Done),
                ]
            });
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

struct GatedSummaryProvider {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for GatedSummaryProvider {
    fn id(&self) -> &str {
        "gated-summary"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::TextDelta("gated summary".into())),
            Ok(StreamEvent::Done),
        ])))
    }
}

struct PartialSlowProvider;

#[async_trait]
impl Provider for PartialSlowProvider {
    fn id(&self) -> &str {
        "partial-slow"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(
            futures_util::stream::iter(vec![Ok(StreamEvent::TextDelta("partial".into()))]).chain(
                futures_util::stream::pending::<Result<StreamEvent, ProviderError>>(),
            ),
        ))
    }
}

#[test]
fn configures_provider_without_persisting_secrets() {
    let dir = TempDir::new().unwrap();
    let engine = Engine::new(Store::open(dir.path()).unwrap());
    let service = AgentService::unconfigured(engine);
    assert!(!service.info().configured);
    let info = service
        .configure(ConfigureAgentParams {
            provider: "openai-compatible".to_string(),
            base_url: Some("http://127.0.0.1:1234/v1/".to_string()),
            model: "local-model".to_string(),
            role_models: None,
            api_key: Some("secret".to_string()),
            max_turns: None,
            max_tokens: None,
            max_managers: None,
            max_workers: None,
            max_labour: None,
            auto_expand_workers: None,
            context_window: None,
            approval_mode: None,
        })
        .unwrap();
    assert!(info.configured);
    assert_eq!(info.model.as_deref(), Some("local-model"));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:1234/v1"));

    let error = service
        .configure(ConfigureAgentParams {
            provider: "openai-compatible".to_string(),
            base_url: None,
            model: "   ".to_string(),
            role_models: None,
            api_key: None,
            max_turns: None,
            max_tokens: None,
            max_managers: None,
            max_workers: None,
            max_labour: None,
            auto_expand_workers: None,
            context_window: None,
            approval_mode: None,
        })
        .unwrap_err();
    assert!(error.to_string().contains("model is required"));
}

struct RoutingProvider {
    active: std::sync::atomic::AtomicUsize,
    max_active: std::sync::atomic::AtomicUsize,
}

impl RoutingProvider {
    fn new() -> Self {
        Self {
            active: std::sync::atomic::AtomicUsize::new(0),
            max_active: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Provider for RoutingProvider {
    fn id(&self) -> &str {
        "routing"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let last_is_tool = request
            .messages
            .last()
            .is_some_and(|message| message.role == "tool");
        let user_text = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let events = if last_is_tool {
            if user_text.contains("parallel goal") {
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ]
            } else {
                vec![
                    StreamEvent::TextDelta("department done".into()),
                    StreamEvent::Done,
                ]
            }
        } else if user_text.contains("parallel goal") {
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Implementation",
                        "objective": "department"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ]
        } else if user_text.contains("department") {
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_many".into(),
                    name: "delegate_many".into(),
                    arguments: serde_json::json!({
                        "tasks": [
                            {
                                "name": "Worker A",
                                "objective": "task A",
                                "write_scope": ["src/a"]
                            },
                            {
                                "name": "Worker B",
                                "objective": "task B",
                                "write_scope": ["src/b"]
                            }
                        ]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ]
        } else {
            let current = self
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.max_active
                .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.active
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let text = if user_text.contains("task A") {
                "A done"
            } else {
                "B done"
            };
            vec![StreamEvent::TextDelta(text.into()), StreamEvent::Done]
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct PlanRoutingProvider {
    active: std::sync::atomic::AtomicUsize,
    max_active: std::sync::atomic::AtomicUsize,
    overlapping_scopes: bool,
}

impl PlanRoutingProvider {
    fn new() -> Self {
        Self::with_overlapping_scopes(false)
    }

    fn with_overlapping_scopes(overlapping_scopes: bool) -> Self {
        Self {
            active: std::sync::atomic::AtomicUsize::new(0),
            max_active: std::sync::atomic::AtomicUsize::new(0),
            overlapping_scopes,
        }
    }
}

#[async_trait]
impl Provider for PlanRoutingProvider {
    fn id(&self) -> &str {
        "plan-routing"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let last_message = request.messages.last();
        let last_role = last_message.map(|message| message.role.as_str());
        let last_content = last_message
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let user_text = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let events = if user_text.contains("Execute plan task T1")
            || user_text.contains("Execute plan task T2")
        {
            let current = self
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.max_active
                .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.active
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let text = if user_text.contains("T1") {
                "T1 done"
            } else {
                "T2 done"
            };
            vec![StreamEvent::TextDelta(text.into()), StreamEvent::Done]
        } else if last_role == Some("tool") {
            if last_content.contains("Plan complete") {
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ]
            } else {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "run_plan".into(),
                        name: "run_team_plan".into(),
                        arguments: "{}".into(),
                    }),
                    StreamEvent::Done,
                ]
            }
        } else {
            let plan = serde_json::json!({
                "goal": "auto plan",
                "departments": [{
                    "name": "research",
                    "manager_title": "Sources",
                    "profile": "research",
                    "objective": "collect sources"
                }],
                "tasks": [
                    {"id": "T1", "department": "research", "title": "Task one", "kind": "research", "deps": [], "acceptance": ["done"], "write_scope": [if self.overlapping_scopes { "src/shared" } else { "src/t1" }]},
                    {"id": "T2", "department": "research", "title": "Task two", "kind": "research", "deps": [], "acceptance": ["done"], "write_scope": [if self.overlapping_scopes { "src/shared" } else { "src/t2" }]}
                ]
            });
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "submit_plan".into(),
                    name: "submit_team_plan".into(),
                    arguments: plan.to_string(),
                }),
                StreamEvent::Done,
            ]
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct BudgetPlanProvider {
    department_tokens: Option<u64>,
}

#[async_trait]
impl Provider for BudgetPlanProvider {
    fn id(&self) -> &str {
        "budget-plan"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let last = request.messages.last();
        let last_content = last
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let user_text = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let events = if user_text.contains("Execute plan task") {
            vec![
                StreamEvent::TextDelta("worker should not run".into()),
                StreamEvent::Done,
            ]
        } else if last_content.contains("Plan complete") {
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ]
        } else if last.map(|message| message.role.as_str()) == Some("tool") {
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "run_plan".into(),
                    name: "run_team_plan".into(),
                    arguments: "{}".into(),
                }),
                StreamEvent::Done,
            ]
        } else {
            let mut plan = serde_json::json!({
                "goal": "budget plan",
                "departments": [{
                    "name": "impl",
                    "manager_title": "Implementation",
                    "profile": "coding",
                    "objective": "build"
                }],
                "tasks": [
                    {"id": "T1", "department": "impl", "title": "One", "kind": "coding", "acceptance": ["done"], "write_scope": ["src/t1"], "token_budget": 1},
                    {"id": "T2", "department": "impl", "title": "Two", "kind": "coding", "acceptance": ["done"], "write_scope": ["src/t2"], "token_budget": 1}
                ]
            });
            if let Some(tokens) = self.department_tokens {
                plan["departments"][0]["token_budget"] = serde_json::json!(tokens);
                plan["tasks"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("token_budget");
                plan["tasks"][1]
                    .as_object_mut()
                    .unwrap()
                    .remove("token_budget");
            }
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "submit_plan".into(),
                    name: "submit_team_plan".into(),
                    arguments: plan.to_string(),
                }),
                StreamEvent::Done,
            ]
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct CancellingPlanProvider;

#[async_trait]
impl Provider for CancellingPlanProvider {
    fn id(&self) -> &str {
        "cancelling-plan"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let last_role = request.messages.last().map(|message| message.role.as_str());
        let user_text = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        if user_text.contains("Execute plan task") {
            return Ok(Box::pin(futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(StreamEvent::TextDelta("late".into()))
            })));
        }
        let events = if last_role == Some("tool") {
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "run_plan".into(),
                    name: "run_team_plan".into(),
                    arguments: "{}".into(),
                }),
                StreamEvent::Done,
            ]
        } else {
            let plan = serde_json::json!({
                "goal": "cancel a batch",
                "departments": [{
                    "name": "impl",
                    "manager_title": "Implementation",
                    "profile": "coding",
                    "objective": "build"
                }],
                "tasks": [
                    {"id": "T1", "department": "impl", "title": "One", "kind": "coding", "deps": [], "acceptance": ["done"], "write_scope": ["src/t1"]},
                    {"id": "T2", "department": "impl", "title": "Two", "kind": "coding", "deps": [], "acceptance": ["done"], "write_scope": ["src/t2"]}
                ]
            });
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "submit_plan".into(),
                    name: "submit_team_plan".into(),
                    arguments: plan.to_string(),
                }),
                StreamEvent::Done,
            ]
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct RoundtableProvider;

#[async_trait]
impl Provider for RoundtableProvider {
    fn id(&self) -> &str {
        "roundtable"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let last = request.messages.last();
        let last_content = last
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let user_text = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let events = if user_text.contains("Manager roundtable, round 1") {
            let title = if user_text.contains("Sources") {
                "Sources"
            } else {
                "Implementation"
            };
            vec![
                StreamEvent::TextDelta(format!("{title} r1 position")),
                StreamEvent::Done,
            ]
        } else if user_text.contains("Manager roundtable, round 2") {
            let title = if user_text.contains("Sources") {
                "Sources"
            } else {
                "Implementation"
            };
            vec![
                StreamEvent::TextDelta(format!("{title} r2 decision")),
                StreamEvent::Done,
            ]
        } else if last.map(|message| message.role.as_str()) == Some("tool")
            && last_content.contains("Team plan recorded")
        {
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "deliberate".into(),
                    name: "deliberate_team_plan".into(),
                    arguments: serde_json::json!({"rounds": 2}).to_string(),
                }),
                StreamEvent::Done,
            ]
        } else if last.map(|message| message.role.as_str()) == Some("tool") {
            vec![
                StreamEvent::TextDelta("deliberation complete".into()),
                StreamEvent::Done,
            ]
        } else {
            let plan = serde_json::json!({
                "goal": "deliberate the plan",
                "departments": [
                    {"name": "sources", "manager_title": "Sources", "profile": "research", "objective": "collect sources"},
                    {"name": "implementation", "manager_title": "Implementation", "profile": "coding", "objective": "implement"}
                ],
                "tasks": [
                    {"id": "T1", "department": "sources", "title": "Research", "kind": "research", "deps": [], "acceptance": ["sources"]},
                    {"id": "T2", "department": "implementation", "title": "Build", "kind": "coding", "deps": ["T1"], "acceptance": ["tests"]}
                ]
            });
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "submit_plan".into(),
                    name: "submit_team_plan".into(),
                    arguments: plan.to_string(),
                }),
                StreamEvent::Done,
            ]
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

fn init_repo(path: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@verlinpark.local"],
        vec!["config", "user.name", "VerlinPark Test"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[tokio::test]
async fn runs_tool_call_and_final_message() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "inspect the repo".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "inspect the repo")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "tool.finished"));
    assert!(events.iter().any(|event| event.kind == "agent.message"));
    let messages = engine.list_messages(&session.id).await.unwrap();
    assert!(messages.iter().any(|message| message.role == "assistant"));
}
#[tokio::test]
async fn smart_policy_auto_approves_workspace_writes() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "notes/smart.txt",
                        "content": "smart policy\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("wrote the file".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "write without a prompt".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "write without a prompt")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let worktree = session.worktree.clone().unwrap();
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&worktree).join("notes/smart.txt")).unwrap(),
        "smart policy\n"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "approval.requested")
    );
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "write_file"
            && event.payload["is_error"] == false
    }));
}

#[tokio::test]
async fn smart_policy_denies_blocked_shell_without_asking() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_sudo".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({"command": "sudo true"}).to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("the host command was refused".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "refuse privilege escalation".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "refuse privilege escalation")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "approval.requested")
    );
    assert!(events.iter().any(|event| {
        event.kind == "permission.denied"
            && event.payload["tool"] == "shell"
            && event.payload["risk"] == "destructive"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "shell"
            && event.payload["is_error"] == true
            && event.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("Blocked by the local permission policy"))
    }));
}

#[tokio::test]
async fn writes_a_persisted_plan() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_todo".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [
                            {"title": "Inspect", "status": "completed"},
                            {"title": "Implement", "status": "in_progress"}
                        ]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("still working".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "plan the work".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "plan the work").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let todos = engine.list_todos(&session.id).await.unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[1].status, "in_progress");
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "todo.updated"));
}

#[tokio::test]
async fn delegates_through_four_tiers() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Discovery",
                        "objective": "handle the department"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "inspect the readme"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_labour".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "labour",
                        "objective": "read the readme"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("labour done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "delegate the work".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "delegate the work")
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let agents = engine.list_agents(&session.id).await.unwrap();
        let roles = agents
            .iter()
            .map(|agent| agent.role.as_str())
            .collect::<Vec<_>>();
        let settled = roles.contains(&"leader")
            && roles.contains(&"manager")
            && roles.contains(&"worker")
            && roles.contains(&"labour")
            && agents
                .iter()
                .filter(|agent| agent.role != "leader")
                .all(|agent| agent.status == "idle");
        if settled {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "agents did not settle: {agents:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.spawned")
            .count(),
        3
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.message" && event.payload["text"] == "leader done")
    );
}

#[tokio::test]
async fn integrates_worker_changes_into_the_session_branch() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Implementation",
                        "objective": "deliver the file"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "write worker.txt",
                        "write_scope": ["worker.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "worker.txt",
                        "content": "from worker\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker wrote".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "worker.txt exists and matches the task"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "integrate".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..100 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let worktree = session.worktree.clone().unwrap();
    service.start(&session.id, "integrate").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read_to_string(std::path::Path::new(&worktree).join("worker.txt"))
            .ok()
            .as_deref()
            == Some("from worker\n")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker changes were not integrated"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&worktree).join("worker.txt")).unwrap(),
        "from worker\n"
    );
    let diff = engine.session_diff(&session.id).await.unwrap();
    assert!(diff.status.is_empty());
    assert!(diff.patch.contains("+from worker"));
    let agents = engine.list_agents(&session.id).await.unwrap();
    let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
    assert_eq!(worker.status, "idle");
    assert!(
        agents
            .iter()
            .any(|agent| agent.role == "verifier" && agent.status == "idle")
    );
    assert!(
        worker
            .branch
            .as_deref()
            .is_some_and(|branch| branch.starts_with("verlinpark/"))
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "worker"
            && event.payload["merged"] == true
    }));
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "merged");
    assert_eq!(queue[0].agent_name, "Worker 1");
    assert!(
        events
            .iter()
            .any(|event| event.kind == "merge_queue.enqueued")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "integrator.started")
    );
    assert!(events.iter().any(
            |event| event.kind == "integrator.finished" && event.payload["status"] == "merged"
        ));
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "idle");
}

#[tokio::test]
async fn records_merge_conflicts_in_the_queue() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let project_dir = dir.path().join("repo");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("README.md"), "initial\n").unwrap();
    init_repo(&project_dir);
    let project = engine
        .add_project(
            project_dir.to_string_lossy().to_string(),
            Some("repo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "conflict".into(), None)
        .await
        .unwrap();
    let branch = session.branch.clone().unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    let task = engine
        .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
        .await
        .unwrap();
    std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
    let commit = worktree::commit_all(&task.path, "worker change")
        .unwrap()
        .unwrap();
    std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
    worktree::commit_all(&session_worktree, "session change").unwrap();

    let spec = AgentSpec {
        id: "worker-test".into(),
        role: AgentRole::Worker,
        name: "Worker test".into(),
        depth: 2,
        parent_name: Some("Manager".into()),
        title: None,
        task_id: Some("T1".into()),
        department: Some("impl".into()),
        profile: AgentProfile::Coding,
        allow_delegation: true,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),
    };
    let error = integrate_worker(&engine, &session, &spec, &commit, "verified", None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("merge conflict"), "{error}");

    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "conflict");
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
        "session\n"
    );
}

#[tokio::test]
async fn retries_a_conflicted_merge_after_rebase() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "retry conflict".into(), None)
        .await
        .unwrap();
    let worker = engine
        .create_agent(&session.id, "worker", "Worker retry", None, None, "test")
        .await
        .unwrap();
    let branch = session.branch.clone().unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    let task = engine
        .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
        .await
        .unwrap();
    engine
        .set_agent_worktree(&worker.id, &task.path.to_string_lossy(), &task.branch)
        .await
        .unwrap();
    std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
    let commit = worktree::commit_all(&task.path, "worker change")
        .unwrap()
        .unwrap();
    std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
    worktree::commit_all(&session_worktree, "session change").unwrap();

    let spec = AgentSpec {
        id: worker.id.clone(),
        role: AgentRole::Worker,
        name: worker.name.clone(),
        depth: 2,
        parent_name: None,
        title: None,
        task_id: Some("T1".into()),
        department: Some("impl".into()),
        profile: AgentProfile::Coding,
        allow_delegation: true,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),
    };
    integrate_worker(&engine, &session, &spec, &commit, "verified", None)
        .await
        .unwrap_err();
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue[0].status, "conflict");

    // Drop the conflicting session commit, then retry: the worker branch can
    // be rebased and merged cleanly.
    worktree::reset_hard(&session_worktree, &task.base_commit).unwrap();
    let retried = service.retry_merge(&queue[0].id).await.unwrap();
    assert_eq!(retried.status, "merged");
    assert_eq!(retried.attempt, 2);
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
        "worker\n"
    );
}

#[tokio::test]
async fn auto_resolves_a_conflicted_merge_with_an_agent() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "resolve_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "README.md",
                        "content": "resolved\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("resolved the README conflict".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "resolve_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "conflict resolved cleanly"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "resolve conflict".into(), None)
        .await
        .unwrap();
    let worker = engine
        .create_agent(&session.id, "worker", "Worker resolve", None, None, "test")
        .await
        .unwrap();
    let branch = session.branch.clone().unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    let task = engine
        .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
        .await
        .unwrap();
    engine
        .set_agent_worktree(&worker.id, &task.path.to_string_lossy(), &task.branch)
        .await
        .unwrap();
    std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
    let commit = worktree::commit_all(&task.path, "worker change")
        .unwrap()
        .unwrap();
    std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
    worktree::commit_all(&session_worktree, "session change").unwrap();

    let spec = AgentSpec {
        id: worker.id.clone(),
        role: AgentRole::Worker,
        name: worker.name.clone(),
        depth: 2,
        parent_name: None,
        title: None,
        task_id: Some("T1".into()),
        department: Some("impl".into()),
        profile: AgentProfile::Coding,
        allow_delegation: true,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),
    };
    integrate_worker(&engine, &session, &spec, &commit, "verified", None)
        .await
        .unwrap_err();
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue[0].status, "conflict");

    let resolved = service.resolve_merge(&queue[0].id).await.unwrap();
    assert_eq!(resolved.status, "merged", "{:?}", resolved.detail);
    assert_eq!(resolved.attempt, 2);
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
        "resolved\n"
    );
    let agents = engine.list_agents(&session.id).await.unwrap();
    assert!(
        agents
            .iter()
            .any(|agent| { agent.name == "Conflict Resolver" && agent.status == "idle" })
    );
    let sessions = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "idle");
}

#[tokio::test]
async fn auto_resolver_policy_resolves_pending_conflicts() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "auto_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "README.md",
                        "content": "resolved by policy\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("policy resolver done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "auto_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "policy resolution verified"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "policy resolve".into(), None)
        .await
        .unwrap();
    let worker = engine
        .create_agent(&session.id, "worker", "Worker policy", None, None, "test")
        .await
        .unwrap();
    let branch = session.branch.clone().unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    let task = engine
        .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
        .await
        .unwrap();
    engine
        .set_agent_worktree(&worker.id, &task.path.to_string_lossy(), &task.branch)
        .await
        .unwrap();
    std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
    let commit = worktree::commit_all(&task.path, "worker change")
        .unwrap()
        .unwrap();
    std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
    worktree::commit_all(&session_worktree, "session change").unwrap();

    let spec = AgentSpec {
        id: worker.id.clone(),
        role: AgentRole::Worker,
        name: worker.name.clone(),
        depth: 2,
        parent_name: None,
        title: None,
        task_id: Some("T1".into()),
        department: Some("impl".into()),
        profile: AgentProfile::Coding,
        allow_delegation: true,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),
    };
    integrate_worker(&engine, &session, &spec, &commit, "verified", None)
        .await
        .unwrap_err();
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue[0].status, "conflict");
    engine
        .set_integration_policy(
            &project.id,
            vp_protocol::types::IntegrationPolicy {
                auto_resolve: true,
                max_auto_attempts: 1,
                max_resolve_tokens: Some(200_000),
            },
        )
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();

    service.auto_resolve_project(&project.id).await;

    let resolved = engine
        .get_merge_queue_entry(&queue[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, "merged", "{:?}", resolved.detail);
    assert_eq!(resolved.attempt, 2);
    assert_eq!(engine.auto_resolve_attempts(&queue[0].id).await.unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
        "resolved by policy\n"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "merge_queue.auto_resolve_started")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "merge_queue.auto_resolve_finished")
    );
}

#[tokio::test]
async fn rolls_back_a_merge_when_integration_checks_fail() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Implementation",
                        "objective": "deliver the file"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "write worker.txt",
                        "write_scope": ["worker.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "worker.txt",
                        "content": "from worker\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker wrote".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "worker.txt exists and matches the task"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_integration_command(&service, Some("exit 7"));
    let session = engine
        .create_session(project.id, "integration failure".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..100 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    service
        .start(&session.id, "integration failure")
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let queue = loop {
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        if queue.len() == 1 && queue[0].status == "failed" {
            break queue;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "integration failure was not recorded: {queue:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    approver.abort();

    let worktree = session.worktree.clone().unwrap();
    assert!(
        !std::path::Path::new(&worktree).join("worker.txt").exists(),
        "a failed integration check must roll the merge back"
    );
    assert_eq!(queue.len(), 1);
    assert!(
        queue[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("integration command exited"))
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let (worker_status, saw_integrator_failure) = loop {
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        let saw_integrator_failure = events.iter().any(|event| {
            event.kind == "integrator.finished" && event.payload["status"] == "failed"
        });
        if worker.status == "failed" && saw_integrator_failure {
            break (worker.status.clone(), saw_integrator_failure);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker/integrator failure projections did not settle: {worker:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(worker_status, "failed");
    assert!(saw_integrator_failure);
}

#[tokio::test]
async fn blocks_integration_when_the_verifier_rejects() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Quality",
                        "objective": "review the change"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "write rejected.txt",
                        "write_scope": ["rejected.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "rejected.txt",
                        "content": "not ready\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker wrote".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": false,
                        "summary": "the acceptance test fails"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "verify".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..100 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    service.start(&session.id, "verify").await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    approver.abort();

    let worktree = session.worktree.clone().unwrap();
    assert!(
        !std::path::Path::new(&worktree)
            .join("rejected.txt")
            .exists()
    );
    let diff = engine.session_diff(&session.id).await.unwrap();
    assert!(diff.patch.is_empty());
    let agents = engine.list_agents(&session.id).await.unwrap();
    let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
    assert_eq!(worker.status, "changes_requested");
    let verifier = agents
        .iter()
        .find(|agent| agent.role == "verifier")
        .unwrap();
    assert_eq!(verifier.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "worker"
            && event.payload["status"] == "changes_requested"
            && event.payload["approved"] == false
    }));
}

#[tokio::test]
async fn rejects_a_verifier_that_modifies_tracked_files() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Quality",
                        "objective": "review the change"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "write verifier-target.txt",
                        "write_scope": ["verifier-target.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "verifier-target.txt",
                        "content": "original\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker wrote".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_shell".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({
                        "command": "printf 'tampered\n' > verifier-target.txt"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "looks good"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "verifier-write".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..200 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    service.start(&session.id, "verifier-write").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    approver.abort();

    let agents = engine.list_agents(&session.id).await.unwrap();
    let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
    assert_eq!(worker.status, "changes_requested");
    let verifier = agents
        .iter()
        .find(|agent| agent.role == "verifier")
        .unwrap();
    assert_eq!(verifier.status, "failed");
    let verifier_worktree = PathBuf::from(verifier.worktree.clone().unwrap());
    assert!(
        !verifier_worktree.exists(),
        "the isolated verification checkout must be removed after the run"
    );
    let session_worktree = session.worktree.clone().unwrap();
    assert!(
        !std::path::Path::new(&session_worktree)
            .join("verifier-target.txt")
            .exists()
    );
    let diff = engine.session_diff(&session.id).await.unwrap();
    assert!(diff.patch.is_empty());
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "verifier"
            && event.payload["approved"] == false
            && event.payload["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("modified tracked files"))
    }));
}

#[test]
fn matches_write_scopes() {
    assert!(path_in_scope("src/a.rs", &["src".to_string()]));
    assert!(path_in_scope("src/a.rs", &["src/**".to_string()]));
    assert!(!path_in_scope("docs/a.md", &["src".to_string()]));
    assert!(path_in_scope("anything", &[]));
    assert!(scopes_overlap(
        &["src".to_string()],
        &["src/components".to_string()]
    ));
    assert!(!scopes_overlap(&["src".to_string()], &["docs".to_string()]));
    assert!(scopes_overlap(&["*".to_string()], &["src".to_string()]));
    assert!(scopes_overlap(&["**".to_string()], &["docs".to_string()]));
}

#[tokio::test]
async fn runs_parallel_workers_in_isolated_scopes() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(RoutingProvider::new());
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "parallel goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "parallel goal").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let agents = engine.list_agents(&session.id).await.unwrap();
        let workers = agents
            .iter()
            .filter(|agent| agent.role == "worker")
            .collect::<Vec<_>>();
        let status = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap()
            .first()
            .map(|session| session.status.clone());
        if workers.len() == 2
            && workers.iter().all(|worker| worker.status == "idle")
            && status.as_deref() == Some("idle")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "parallel workers did not finish; agents={agents:?}, session={status:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        provider
            .max_active
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2,
        "workers did not run concurrently"
    );
    let agents = engine.list_agents(&session.id).await.unwrap();
    let workers = agents
        .iter()
        .filter(|agent| agent.role == "worker")
        .collect::<Vec<_>>();
    assert_eq!(workers.len(), 2);
    assert!(workers.iter().all(|worker| worker.status == "idle"));
    assert!(workers.iter().all(|worker| worker.branch.is_some()));
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.spawned")
            .count(),
        3
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.message" && event.payload["text"] == "A done")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.message" && event.payload["text"] == "B done")
    );
    let sessions = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "idle");
}

#[tokio::test]
async fn rejects_worker_changes_outside_the_write_scope() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Quality",
                        "objective": "deliver"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "write outside the scope",
                        "write_scope": ["src"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "docs/out.txt",
                        "content": "outside\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker wrote".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "scope".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..100 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    service.start(&session.id, "scope").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    approver.abort();

    let worktree = session.worktree.clone().unwrap();
    assert!(
        !std::path::Path::new(&worktree)
            .join("docs/out.txt")
            .exists()
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        if worker.status == "failed" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker status did not settle: {worker:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "worker"
            && event.payload["status"] == "failed"
            && event.payload["summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("write scope violation"))
    }));
}

#[tokio::test]
async fn requires_a_title_for_managers() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "objective": "unowned department"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("no manager".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "title check".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "title check").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let agents = engine.list_agents(&session.id).await.unwrap();
    assert!(!agents.iter().any(|agent| agent.role == "manager"));
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "delegate"
            && event.payload["is_error"] == true
    }));
}

#[tokio::test]
async fn submits_a_team_plan_and_applies_manager_profiles() {
    let dir = TempDir::new().unwrap();
    let plan = serde_json::json!({
        "goal": "research agent platforms",
        "departments": [{
            "name": "sources",
            "manager_title": "Sources",
            "profile": "research",
            "objective": "collect primary sources"
        }],
        "tasks": [{
            "id": "T1",
            "department": "sources",
            "title": "Collect sources",
            "kind": "research",
            "deps": [],
            "write_scope": [],
            "acceptance": ["at least three primary sources"]
        }]
    });
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_plan".into(),
                    name: "submit_team_plan".into(),
                    arguments: plan.to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_sources".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Sources",
                        "objective": "collect primary sources"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("sources collected".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("research complete".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "research".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "research").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let stored = engine.get_team_plan(&session.id).await.unwrap().unwrap();
    assert_eq!(stored.departments[0].manager_title, "Sources");
    assert_eq!(stored.tasks[0].id, "T1");
    let agents = engine.list_agents(&session.id).await.unwrap();
    let manager = agents.iter().find(|agent| agent.role == "manager").unwrap();
    assert_eq!(manager.title.as_deref(), Some("Sources"));
    assert_eq!(manager.profile.as_deref(), Some("research"));
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "team_plan.submitted")
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "manager"
            && event.payload["agent_profile"] == "research"
    }));
}

#[tokio::test]
async fn rejects_cyclic_team_plans() {
    let dir = TempDir::new().unwrap();
    let plan = serde_json::json!({
        "goal": "bad plan",
        "departments": [{
            "name": "impl",
            "manager_title": "Implementation",
            "profile": "coding",
            "objective": "build"
        }],
        "tasks": [
            {"id": "A", "department": "impl", "title": "A", "kind": "coding", "deps": ["B"], "acceptance": ["done"]},
            {"id": "B", "department": "impl", "title": "B", "kind": "coding", "deps": ["A"], "acceptance": ["done"]}
        ]
    });
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_plan".into(),
                    name: "submit_team_plan".into(),
                    arguments: plan.to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("plan rejected".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "cycle".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "cycle").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(engine.get_team_plan(&session.id).await.unwrap().is_none());
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "submit_team_plan"
            && event.payload["is_error"] == true
    }));
}

#[tokio::test]
async fn resumes_persisted_queued_sessions_without_duplicating_the_prompt() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("resumed queue".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "queued".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "user",
            "continue the queue",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "queued")
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "agent.queued",
            serde_json::json!({"prompt": "continue the queue"}),
        )
        .await
        .unwrap();

    assert_eq!(service.resume_queued_sessions().await.unwrap(), 1);
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let messages = engine.list_messages(&session.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.content == "continue the queue")
            .count(),
        1
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "resumed queue"
    }));
}

#[tokio::test]
async fn resumes_only_unfinished_plan_tasks() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("T2 done".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "resume plan".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "resume plan".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![
            PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "One".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/t1".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
            PlanTask {
                id: "T2".into(),
                department: "impl".into(),
                title: "Two".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/t2".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
        ],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    engine
        .append_event(
            &session.id,
            "plan_task.finished",
            serde_json::json!({"task_id": "T1", "task_status": "completed"}),
        )
        .await
        .unwrap();

    assert!(service.resume_plan(&session.id).await.unwrap());
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let started = events
        .iter()
        .filter(|event| event.kind == "plan_task.started")
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 1, "completed T1 must be skipped");
    assert_eq!(started[0].payload["task_id"], "T2");
    assert!(events.iter().any(|event| {
        event.kind == "plan_task.finished"
            && event.payload["task_id"] == "T2"
            && event.payload["task_status"] == "completed"
    }));
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.plan_resumed")
    );
    let queue = engine
        .task_queue(Some(sessions.project_id.clone()), Some(sessions.id.clone()))
        .await
        .unwrap();
    assert_eq!(queue.tasks.len(), 2);
    assert!(
        queue.tasks.iter().all(|task| task.status == "completed"),
        "the persistent task queue must reflect the resumed plan: {:?}",
        queue.tasks
    );
    assert!(
        queue
            .tasks
            .iter()
            .find(|task| task.task_id == "T2")
            .unwrap()
            .attempts
            >= 1
    );
}

#[tokio::test]
async fn reuses_an_existing_task_commit_when_resuming() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("continued existing work".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "existing commit is correct"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "resume commit".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "resume commit".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![PlanTask {
            id: "T1".into(),
            department: "impl".into(),
            title: "One".into(),
            kind: "coding".into(),
            priority: 0,
            profile: None,
            deps: Vec::new(),
            write_scope: vec!["worker.txt".into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let session_branch = session.branch.clone().unwrap();
    let task = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            "T1",
            &session_branch,
        )
        .await
        .unwrap();
    std::fs::write(task.path.join("worker.txt"), "already committed\n").unwrap();
    worktree::commit_all(&task.path, "prior task commit").unwrap();

    assert!(service.resume_plan(&session.id).await.unwrap());
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("worker.txt")).unwrap(),
        "already committed\n"
    );
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "merged");
}

#[tokio::test]
async fn settles_sibling_tasks_when_a_batch_is_cancelled() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(CancellingPlanProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "cancel a plan batch".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "cancel a plan batch")
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        if events
            .iter()
            .filter(|event| event.kind == "plan_task.started")
            .count()
            == 2
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "plan tasks did not start"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(service.cancel(&session.id));
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "cancelled");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let finished = events
        .iter()
        .filter(|event| event.kind == "plan_task.finished")
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 2, "unreported sibling tasks: {finished:?}");
    assert!(
        finished
            .iter()
            .all(|event| event.payload["task_status"] == "cancelled"),
        "all sibling tasks must settle as cancelled: {finished:?}"
    );
}

#[tokio::test]
async fn task_budget_blocks_only_the_task_not_the_plan() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(BudgetPlanProvider {
        department_tokens: None,
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "task budgets".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "task budgets").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    let budget_events = events
        .iter()
        .filter(|event| event.kind == "agent.budget_exceeded")
        .collect::<Vec<_>>();
    assert_eq!(
        budget_events
            .iter()
            .filter(|event| event.payload["scope"] == "task")
            .count(),
        2
    );
    assert!(budget_events.iter().all(|event| {
        event.payload["reason"] == "tokens"
            && event.payload["limit"] == 1
            && event.payload["spent"] == 0
    }));
    let blocked = events
        .iter()
        .filter(|event| {
            event.kind == "plan_task.finished"
                && event.payload["task_status"] == "blocked"
                && event.payload["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("task T"))
        })
        .count();
    assert_eq!(blocked, 2);
    assert!(events.iter().any(|event| {
        event.kind == "agent.updated"
            && event.payload["agent"]["role"] == "leader"
            && event.payload["status"] == "idle"
    }));
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert!(
        ledger.entries.iter().all(|entry| entry.task_id.is_none()),
        "blocked tasks must not record usage"
    );
}

#[tokio::test]
async fn department_budget_aggregates_across_ready_tasks() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(BudgetPlanProvider {
        department_tokens: Some(1),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "department budgets".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "department budgets")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    let department_stops = events
        .iter()
        .filter(|event| {
            event.kind == "agent.budget_exceeded"
                && event.payload["scope"] == "department"
                && event.payload["id"] == "impl"
        })
        .count();
    assert_eq!(department_stops, 2);
    assert!(events.iter().any(|event| {
        event.kind == "plan_task.finished"
            && event.payload["task_status"] == "blocked"
            && event.payload["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("department impl"))
    }));
}

async fn wait_until_idle(engine: &Engine, session: &Session) -> vp_protocol::types::Session {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        let current = sessions
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap();
        if matches!(current.status.as_str(), "idle" | "failed" | "cancelled") {
            return current;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session did not finish: {}",
            current.status
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn auto_dispatches_a_team_plan_in_dependency_batches() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(PlanRoutingProvider::new());
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "auto plan".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "auto plan").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        if sessions[0].status == "idle" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "auto plan did not finish: {}",
            sessions[0].status
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        provider
            .max_active
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 2,
        "plan tasks did not run concurrently"
    );
    let plan = engine.get_team_plan(&session.id).await.unwrap().unwrap();
    assert_eq!(plan.tasks.len(), 2);
    let agents = engine.list_agents(&session.id).await.unwrap();
    let manager = agents.iter().find(|agent| agent.role == "manager").unwrap();
    assert_eq!(manager.title.as_deref(), Some("Sources"));
    assert_eq!(manager.profile.as_deref(), Some("research"));
    let workers = agents
        .iter()
        .filter(|agent| agent.role == "worker")
        .collect::<Vec<_>>();
    assert_eq!(workers.len(), 2);
    assert!(workers.iter().all(|worker| worker.status == "idle"));
    assert!(
        workers
            .iter()
            .all(|worker| worker.profile.as_deref() == Some("research"))
    );
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "plan_task.started")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.kind == "plan_task.finished" && event.payload["task_status"] == "completed"
            })
            .count(),
        2
    );
    let sessions = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "idle");
}

#[tokio::test]
async fn serializes_plan_tasks_with_overlapping_scopes() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(PlanRoutingProvider::with_overlapping_scopes(true));
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "overlapping plan".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "overlapping plan")
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        let completed = events
            .iter()
            .filter(|event| {
                event.kind == "plan_task.finished" && event.payload["task_status"] == "completed"
            })
            .count();
        if completed == 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "overlapping plan did not finish; events={events:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert_eq!(
        provider
            .max_active
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "overlapping write scopes must not run concurrently"
    );
}

#[tokio::test]
async fn plan_batches_respect_the_worker_cap() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(PlanRoutingProvider::new());
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    set_worker_cap(&service, 1);
    let session = engine
        .create_session(project.id, "capped plan".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "capped plan").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let events = loop {
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        let completed = events
            .iter()
            .filter(|event| {
                event.kind == "plan_task.finished" && event.payload["task_status"] == "completed"
            })
            .count();
        if completed == 2 {
            break events;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "plan did not finish both tasks; events={events:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    assert_eq!(
        provider
            .max_active
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the worker cap must bound plan concurrency"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.kind == "plan_task.finished" && event.payload["task_status"] == "completed"
            })
            .count(),
        2
    );
}

#[tokio::test]
async fn managers_deliberate_with_peer_positions() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(RoundtableProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "deliberate".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "deliberate").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let (agents, events, sessions) = loop {
        let agents = engine.list_agents(&session.id).await.unwrap();
        let managers = agents
            .iter()
            .filter(|agent| agent.role == "manager")
            .count();
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        let positions = events
            .iter()
            .filter(|event| event.kind == "manager.roundtable")
            .count();
        let finished = events.iter().any(|event| {
            event.kind == "agent.message" && event.payload["text"] == "deliberation complete"
        });
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        if managers >= 2 && positions >= 4 && finished && sessions[0].status == "idle" {
            break (agents, events, sessions);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "roundtable did not complete: managers={managers}, positions={positions}, finished={finished}, status={}",
            sessions[0].status
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let managers = agents
        .iter()
        .filter(|agent| agent.role == "manager")
        .collect::<Vec<_>>();
    assert_eq!(managers.len(), 2);
    let positions = events
        .iter()
        .filter(|event| event.kind == "manager.roundtable")
        .collect::<Vec<_>>();
    assert_eq!(positions.len(), 4);
    assert!(positions.iter().any(|event| event.payload["round"] == 1));
    assert!(positions.iter().any(|event| event.payload["round"] == 2));
    assert!(
        positions
            .iter()
            .any(|event| event.payload["position"].as_str() == Some("Sources r2 decision"))
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "deliberation complete"
    }));
    assert!(
        !events.iter().any(|event| event.kind == "plan_task.started"),
        "deliberation should not execute plan tasks"
    );
    assert_eq!(sessions[0].status, "idle");
}

#[tokio::test]
async fn usage_ledger_groups_by_role_agent_and_task() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "cost ledger".into(), None)
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &session.id,
            "worker-1",
            "Worker One",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "worker-model",
            100,
            50,
            false,
            0.25,
        )
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &session.id,
            "worker-2",
            "Worker Two",
            "worker",
            Some("T2".into()),
            Some("impl".into()),
            "worker-model",
            40,
            20,
            true,
            0.10,
        )
        .await
        .unwrap();
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 2);
    assert_eq!(ledger.totals.calls, 2);
    assert_eq!(ledger.totals.input_tokens, 140);
    assert_eq!(ledger.totals.output_tokens, 70);
    assert!((ledger.totals.cost_usd - 0.35).abs() < 1e-9);
    assert_eq!(ledger.totals.by_role.len(), 1);
    assert_eq!(ledger.totals.by_role[0].key, "worker");
    assert_eq!(ledger.totals.by_role[0].calls, 2);
    assert_eq!(ledger.totals.by_task.len(), 2);
    assert_eq!(ledger.totals.by_department.len(), 1);
    assert_eq!(ledger.totals.by_department[0].key, "impl");
    assert_eq!(ledger.totals.by_department[0].calls, 2);
    assert_eq!(ledger.totals.by_agent.len(), 2);
    assert!(ledger.entries[1].estimated);
    let sessions = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap();
    assert_eq!(sessions[0].tokens_in, 140);
    assert_eq!(sessions[0].tokens_out, 70);
    assert!((sessions[0].cost_spent_usd - 0.35).abs() < 1e-9);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.usage")
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.cost")
            .count(),
        2
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.usage"
            && event.payload["agent_role"] == "worker"
            && event.payload["task_id"] == "T1"
            && event.payload["model"] == "worker-model"
    }));
}

#[test]
fn plan_budget_scope_includes_the_project_pool() {
    let spec = AgentSpec {
        id: "worker-1".into(),
        role: AgentRole::Worker,
        name: "Worker".into(),
        depth: 2,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Coding,
        allow_delegation: false,
        auto_approve: false,
        worktree: PathBuf::from("/tmp"),
        branch: "task".into(),
        session_worktree: PathBuf::from("/tmp"),
        session_branch: "session".into(),
        write_scope: Vec::new(),
        base_commit: "base".into(),
    };
    let plan = TeamPlan {
        goal: "project budget".into(),
        departments: Vec::new(),
        tasks: Vec::new(),
    };
    let scope = PlanBudgetScope::from_plan(
        Some(&plan),
        &spec,
        Some(&vp_protocol::types::ProjectBudget {
            token_budget: Some(100),
            cost_budget_usd: Some(1.0),
        }),
        "project-1",
    );
    assert!(scope.project_active());
    let requests = scope.requests("session-1", (0, 0.0), (0, 0.0), (0, 0.0), 0.0, 0.0);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].pool_key, "project:project-1");
    assert_eq!(requests[0].scope, "project");
    let stop = scope.after((0, 0.0), (0, 0.0), (100, 0.0)).unwrap();
    assert_eq!(stop.scope, "project");
    assert_eq!(stop.reason, "tokens");
}

#[tokio::test]
async fn warns_once_at_eighty_percent_of_token_budget() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "warn budget".into(), Some(1000))
        .await
        .unwrap();
    engine.record_usage(&session.id, 800, 0).await.unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "warn budget").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let warnings = events
        .iter()
        .filter(|event| event.kind == "agent.budget_warning" && event.payload["reason"] == "tokens")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].payload["limit"], 1000);
    assert_eq!(warnings[0].payload["spent"], 800);
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.budget_exceeded")
    );
}

#[tokio::test]
async fn stops_when_token_budget_is_exhausted() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "stay in budget".into(), Some(100))
        .await
        .unwrap();
    engine.record_usage(&session.id, 80, 40).await.unwrap();
    service.start(&session.id, "stay in budget").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.budget_exceeded")
    );
    let sessions = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "failed");
    assert_eq!(sessions[0].tokens_in, 80);
    assert_eq!(sessions[0].tokens_out, 40);
}

#[tokio::test]
async fn persists_estimated_usage_when_provider_omits_usage() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("estimated".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "estimate".into(), Some(100_000))
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "estimate").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        if sessions[0].status == "idle" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "estimated usage run did not finish: {}",
            sessions[0].status
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.entries[0].role, "leader");
    assert_eq!(ledger.entries[0].model, "test");
    assert!(ledger.entries[0].estimated);
    assert!(ledger.entries[0].input_tokens > 0);
    assert!(ledger.entries[0].output_tokens > 0);

    let session = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert!(session.tokens_in > 0);
    assert!(session.tokens_out > 0);
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| { event.kind == "agent.usage" && event.payload["estimated"] == true })
    );
}

#[test]
fn team_plan_validates_task_and_department_budgets() {
    let base = serde_json::json!({
        "goal": "budgets",
        "departments": [{
            "name": "impl",
            "manager_title": "Implementation",
            "profile": "coding",
            "objective": "build",
            "token_budget": 100,
            "cost_budget_usd": 1.0
        }],
        "tasks": [{
            "id": "T1",
            "department": "impl",
            "title": "Task",
            "kind": "coding",
            "acceptance": ["done"],
            "token_budget": 40,
            "cost_budget_usd": 0.4
        }]
    });
    let plan: TeamPlan = serde_json::from_value(base.clone()).unwrap();
    validate_team_plan(&plan, 5).unwrap();

    let mut zero_department = base.clone();
    zero_department["departments"][0]["token_budget"] = serde_json::json!(0);
    assert!(validate_team_plan(&serde_json::from_value(zero_department).unwrap(), 5).is_err());

    let mut zero_task = base.clone();
    zero_task["tasks"][0]["cost_budget_usd"] = serde_json::json!(0);
    assert!(validate_team_plan(&serde_json::from_value(zero_task).unwrap(), 5).is_err());

    let mut task_over_department = base.clone();
    task_over_department["tasks"][0]["token_budget"] = serde_json::json!(101);
    assert!(validate_team_plan(&serde_json::from_value(task_over_department).unwrap(), 5).is_err());

    let mut sum_over_department = base;
    sum_over_department["tasks"] = serde_json::json!([
        {"id": "T1", "department": "impl", "title": "One", "kind": "coding", "acceptance": ["done"], "token_budget": 60},
        {"id": "T2", "department": "impl", "title": "Two", "kind": "coding", "acceptance": ["done"], "token_budget": 60}
    ]);
    assert!(validate_team_plan(&serde_json::from_value(sum_over_department).unwrap(), 5).is_err());
}

#[test]
fn plan_batch_preserves_scheduler_ready_order() {
    fn task(id: &str, priority: i32, scope: &[&str]) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            department: "impl".to_string(),
            title: id.to_string(),
            kind: "coding".to_string(),
            priority,
            profile: None,
            deps: Vec::new(),
            write_scope: scope.iter().map(|scope| scope.to_string()).collect(),
            acceptance: vec!["done".to_string()],
            token_budget: None,
            cost_budget_usd: None,
        }
    }
    let batch = select_plan_batch(
        vec![
            task("T2", 50, &["src/b"]),
            task("T1", 0, &["src/a"]),
            task("T3", -10, &["src/c"]),
        ],
        10,
    );
    assert_eq!(
        batch
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["T2", "T1", "T3"]
    );
}

#[test]
fn plan_batch_serializes_unscoped_and_overlapping_tasks() {
    fn task(id: &str, scope: &[&str]) -> PlanTask {
        PlanTask {
            id: id.to_string(),
            department: "impl".to_string(),
            title: id.to_string(),
            kind: "coding".to_string(),
            priority: 0,
            profile: None,
            deps: Vec::new(),
            write_scope: scope.iter().map(|scope| scope.to_string()).collect(),
            acceptance: vec!["done".to_string()],
            token_budget: None,
            cost_budget_usd: None,
        }
    }

    let batch = select_plan_batch(
        vec![task("T1", &[]), task("T2", &["src"]), task("T3", &["src"])],
        10,
    );
    assert_eq!(
        batch
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["T1"]
    );

    let batch = select_plan_batch(
        vec![
            task("T1", &["src"]),
            task("T2", &["docs"]),
            task("T3", &["src/sub"]),
        ],
        10,
    );
    assert_eq!(
        batch
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["T1", "T2"]
    );
}

#[test]
fn session_budget_tracks_cost_and_time() {
    let caps = AgentCaps {
        managers: 5,
        workers: 10,
        labour: 20,
    };
    let cost_budget = SessionBudget::new(None, 0, caps)
        .with_cost(Some(0.01), 0.0)
        .with_time(Some(60));
    assert!(!cost_budget.exhausted());
    assert!(cost_budget.add_cost(0.02));
    assert_eq!(cost_budget.exceeded_reason(), Some("cost"));

    let time_budget = SessionBudget::new(None, 0, caps).with_time(Some(0));
    assert_eq!(time_budget.exceeded_reason(), Some("time"));
    assert_eq!(usage_cost(1_000_000, 500_000, 2.0, 4.0), 4.0);
}

#[test]
fn token_reservations_bound_concurrent_requests() {
    let budget = SessionBudget::new(
        Some(1000),
        0,
        AgentCaps {
            managers: 5,
            workers: 10,
            labour: 20,
        },
    );
    let first = budget.reserve_tokens(600).expect("first reservation");
    assert!(budget.reserve_tokens(500).is_none());
    assert_eq!(budget.available_tokens(), Some(400));
    assert!(!first.settle(300));
    assert_eq!(budget.spent(), 300);

    {
        let _unused = budget.reserve_tokens(200).expect("temporary reservation");
    }
    assert_eq!(budget.available_tokens(), Some(700));

    let second = budget.reserve_tokens(700).expect("second reservation");
    assert!(second.settle(700));
    assert_eq!(budget.spent(), 1000);
    assert!(budget.reserve_tokens(1).is_none());
}

struct PausingProvider {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Provider for PausingProvider {
    fn id(&self) -> &str {
        "pausing"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "pause-todo".into(),
                    name: "todo_write".into(),
                    arguments: serde_json::json!({
                        "items": [{"title": "Resume", "status": "in_progress"}]
                    })
                    .to_string(),
                })),
                Ok(StreamEvent::Done),
            ])))
        } else {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("resumed".into())),
                Ok(StreamEvent::Done),
            ])))
        }
    }
}

#[tokio::test]
async fn mailbox_tools_send_read_and_publish() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "mailbox".into(), None)
        .await
        .unwrap();
    let leader = engine
        .create_agent(&session.id, "leader", "Leader", None, None, "test")
        .await
        .unwrap();
    let worker = engine
        .create_agent(&session.id, "worker", "Worker", None, None, "test")
        .await
        .unwrap();
    let spec = AgentSpec {
        id: leader.id.clone(),
        role: AgentRole::Leader,
        name: leader.name.clone(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        auto_approve: false,
        worktree: PathBuf::from(session.worktree.clone().unwrap()),
        branch: session.branch.clone().unwrap(),
        session_worktree: PathBuf::from(session.worktree.clone().unwrap()),
        session_branch: session.branch.clone().unwrap(),
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap(),
    };
    let sent = run_mailbox_tool(
        &engine,
        &session,
        &spec,
        "send_message",
        serde_json::json!({
            "to": worker.name,
            "kind": "handoff",
            "body": "Please build the API",
            "payload": {"task": "T1"}
        }),
    )
    .await
    .unwrap();
    assert!(!sent.is_error, "{}", sent.content);
    let worker_spec = AgentSpec {
        id: worker.id.clone(),
        name: worker.name.clone(),
        ..spec.clone()
    };
    let read = run_mailbox_tool(
        &engine,
        &session,
        &worker_spec,
        "read_mailbox",
        serde_json::json!({"unread_only": true}),
    )
    .await
    .unwrap();
    assert!(
        read.content.contains("Please build the API"),
        "{}",
        read.content
    );
    let published = run_mailbox_tool(
        &engine,
        &session,
        &worker_spec,
        "publish_artifact",
        serde_json::json!({
            "name": "worker-notes.md",
            "kind": "note",
            "content": "# notes"
        }),
    )
    .await
    .unwrap();
    assert!(!published.is_error, "{}", published.content);
    let artifacts = run_mailbox_tool(
        &engine,
        &session,
        &worker_spec,
        "list_artifacts",
        serde_json::json!({}),
    )
    .await
    .unwrap();
    assert!(artifacts.content.contains("worker-notes.md"));
}

#[tokio::test]
async fn role_models_override_the_base_model() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.role_models.worker.model = Some("worker-model".into());
    config.role_models.worker.input_usd_per_million = Some(2.0);
    config.role_models.worker.output_usd_per_million = Some(4.0);
    assert_eq!(model_for(&config, AgentRole::Worker), "worker-model");
    assert_eq!(model_for(&config, AgentRole::Manager), "test");
    assert_eq!(role_price(&config, AgentRole::Worker), (2.0, 4.0));
}

#[tokio::test]
async fn probes_the_configured_provider() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("OK".into()),
            StreamEvent::Done,
        ]])),
    });
    let (_engine, service, _project) = test_service_with_provider(&dir, provider);
    let result = service.probe().await.unwrap();
    assert_eq!(result.provider, "queue");
    assert_eq!(result.model, "test");
    assert_eq!(result.text, "OK");
    assert!(result.latency_ms < 5_000);
    assert_eq!(result.chunks, 1);
    assert!(result.first_token_ms <= result.latency_ms);
    assert!(result.done);
}

#[tokio::test]
async fn runs_a_plan_through_the_openai_http_adapter() {
    use vp_providers::OpenAiCompatibleProvider;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn text_sse(text: &str) -> ResponseTemplate {
        let chunk = serde_json::json!({
            "choices": [{"delta": {"content": text}}]
        });
        ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {chunk}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
                ))
    }

    fn tool_sse(id: &str, name: &str, arguments: serde_json::Value) -> ResponseTemplate {
        let chunk = serde_json::json!({
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": id,
                "function": {"name": name, "arguments": arguments.to_string()}
            }]}}]
        });
        ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {chunk}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
                ))
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let system = messages
                .iter()
                .find(|message| message["role"] == "system")
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default();
            let last = messages.last();
            let last_role = last
                .and_then(|message| message["role"].as_str())
                .unwrap_or_default();
            let last_content = last
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default();

            if system.contains("You are Verifier") {
                return tool_sse(
                    "verdict",
                    "submit_verdict",
                    serde_json::json!({
                        "approved": true,
                        "summary": "HTTP worker output is correct"
                    }),
                );
            }
            if system.contains("You are Worker") {
                if last_role == "tool" {
                    return text_sse("worker done");
                }
                return tool_sse(
                    "write",
                    "write_file",
                    serde_json::json!({"path": "worker.txt", "content": "http e2e\n"}),
                );
            }
            if last_role == "tool" {
                if last_content.contains("Plan complete") {
                    return text_sse("leader done");
                }
                return tool_sse("run_plan", "run_team_plan", serde_json::json!({}));
            }
            let plan = serde_json::json!({
                "goal": "http e2e",
                "departments": [{
                    "name": "impl",
                    "manager_title": "Implementation",
                    "profile": "coding",
                    "objective": "build"
                }],
                "tasks": [{
                    "id": "T1",
                    "department": "impl",
                    "title": "Write the file",
                    "kind": "coding",
                    "deps": [],
                    "acceptance": ["done"],
                    "write_scope": ["worker.txt"]
                }]
            });
            tool_sse("submit_plan", "submit_team_plan", plan)
        })
        .mount(&server)
        .await;

    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::local(
        "openai",
        format!("{}/v1", server.uri()),
    ));
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "http e2e".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..200 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    service
        .start(&session.id, "write worker.txt through HTTP")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(sessions.status, "idle");

    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    assert_eq!(
        std::fs::read_to_string(worktree.join("worker.txt")).unwrap(),
        "http e2e\n"
    );
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "merged");
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "plan_task.finished" && event.payload["task_status"] == "completed"
    }));
    assert!(
        events
            .iter()
            .any(|event| event.kind == "integrator.finished")
    );
}

#[tokio::test]
async fn runs_a_plan_through_the_anthropic_http_adapter() {
    use vp_providers::AnthropicProvider;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn text_sse(text: &str) -> ResponseTemplate {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        });
        let done = serde_json::json!({
            "type": "message_delta",
            "usage": {"output_tokens": 2}
        });
        ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "event: content_block_delta\ndata: {chunk}\n\nevent: message_delta\ndata: {done}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
                ))
    }

    fn tool_sse(id: &str, name: &str, arguments: serde_json::Value) -> ResponseTemplate {
        let start = serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}
        });
        let delta = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": arguments.to_string()}
        });
        let stop = serde_json::json!({"type": "content_block_stop", "index": 0});
        let done = serde_json::json!({
            "type": "message_delta",
            "usage": {"output_tokens": 2}
        });
        ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "event: content_block_start\ndata: {start}\n\nevent: content_block_delta\ndata: {delta}\n\nevent: content_block_stop\ndata: {stop}\n\nevent: message_delta\ndata: {done}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
                ))
    }

    fn content_text(content: &serde_json::Value) -> String {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str().or_else(|| block["content"].as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let system = body["system"].as_str().unwrap_or_default();
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let last = messages.last();
            let last_content = last
                .map(|message| content_text(&message["content"]))
                .unwrap_or_default();
            let tool_result = last
                .map(|message| message["content"].is_array())
                .unwrap_or(false);

            if system.contains("You are Verifier") {
                return tool_sse(
                    "verdict",
                    "submit_verdict",
                    serde_json::json!({
                        "approved": true,
                        "summary": "Anthropic worker output is correct"
                    }),
                );
            }
            if system.contains("You are Worker") {
                if tool_result {
                    return text_sse("worker done");
                }
                return tool_sse(
                    "write",
                    "write_file",
                    serde_json::json!({"path": "worker.txt", "content": "anthropic e2e\n"}),
                );
            }
            if tool_result {
                if last_content.contains("Plan complete") {
                    return text_sse("leader done");
                }
                return tool_sse("run_plan", "run_team_plan", serde_json::json!({}));
            }
            let plan = serde_json::json!({
                "goal": "anthropic e2e",
                "departments": [{
                    "name": "impl",
                    "manager_title": "Implementation",
                    "profile": "coding",
                    "objective": "build"
                }],
                "tasks": [{
                    "id": "T1",
                    "department": "impl",
                    "title": "Write the file",
                    "kind": "coding",
                    "deps": [],
                    "acceptance": ["done"],
                    "write_scope": ["worker.txt"]
                }]
            });
            tool_sse("submit_plan", "submit_team_plan", plan)
        })
        .mount(&server)
        .await;

    let provider: Arc<dyn Provider> = Arc::new(AnthropicProvider::with_base_url(
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("test-key".to_string()),
    ));
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "anthropic e2e".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..200 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    service
        .start(&session.id, "write worker.txt through Anthropic")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(sessions.status, "idle");
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    assert_eq!(
        std::fs::read_to_string(worktree.join("worker.txt")).unwrap(),
        "anthropic e2e\n"
    );
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "merged");
}

#[tokio::test]
async fn asks_the_user_and_continues_after_an_answer() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "ask_1".into(),
                    name: "ask_user".into(),
                    arguments: serde_json::json!({
                        "question": "Proceed with the migration?",
                        "options": ["yes", "no"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("continued after answer".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "ask".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "ask before migrating")
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let question = loop {
        let questions = engine
            .list_questions(Some(session.id.clone()))
            .await
            .unwrap()
            .questions;
        if let Some(question) = questions.into_iter().find(|item| item.status == "pending") {
            break question;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "question was not requested"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(question.question, "Proceed with the migration?");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let sessions = loop {
        let sessions = engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap();
        if sessions[0].status == "waiting_input" {
            break sessions;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session did not enter waiting_input: {}",
            sessions[0].status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(sessions[0].status, "waiting_input");
    assert!(engine.answer_question(&question.id, "yes").await.unwrap());

    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "ask_user"
            && event.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("User answered: yes"))
    }));
    assert!(events.iter().any(|event| event.kind == "question.answered"));
}

#[tokio::test]
async fn lists_and_calls_mcp_tools_from_an_agent() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("mcp_server.py");
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
        result = {"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "mock", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": message.get("params", {}).get("arguments", {}).get("text", "")}]}
    elif method == "resources/list":
        result = {"resources": [{"uri": "file:///notes.md", "name": "notes", "description": "Project notes", "mimeType": "text/markdown"}]}
    elif method == "resources/templates/list":
        result = {"resourceTemplates": []}
    elif method == "resources/read":
        result = {"contents": [{"uri": "file:///notes.md", "mimeType": "text/markdown", "text": "hello resource"}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message.get("id"), "result": result}), flush=True)
"#,
        )
        .unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "mcp_list".into(),
                    name: "mcp".into(),
                    arguments: serde_json::json!({
                        "action": "list",
                        "server": "mock"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "mcp_call".into(),
                    name: "mcp".into(),
                    arguments: serde_json::json!({
                        "action": "call",
                        "server": "mock",
                        "tool": "echo",
                        "arguments": {"text": "hello mcp"}
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "mcp_resources".into(),
                    name: "mcp".into(),
                    arguments: serde_json::json!({
                        "action": "resources",
                        "server": "mock"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("mcp done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .store()
        .add_mcp_server(&vp_protocol::types::McpServerConfig {
            name: "mock".into(),
            transport: Default::default(),
            command: "python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
        })
        .unwrap();
    let session = engine
        .create_session(project.id, "mcp tools".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..150 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    service.start(&session.id, "use mcp").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "mcp"
            && event.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("hello mcp"))
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "mcp"
            && event.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("notes <file:///notes.md>"))
    }));
}

#[tokio::test]
async fn exposes_mcp_tools_as_dynamic_agent_tools() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("dynamic_mcp_server.py");
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
        result = {"protocolVersion": "2024-11-05", "capabilities": {}, "serverInfo": {"name": "mock", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": message.get("params", {}).get("arguments", {}).get("text", "")}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message.get("id"), "result": result}), flush=True)
"#,
        )
        .unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "dynamic_mcp".into(),
                    name: "mcp_mock_echo".into(),
                    arguments: serde_json::json!({"text": "dynamic mcp"}).to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("dynamic done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .store()
        .add_mcp_server(&vp_protocol::types::McpServerConfig {
            name: "mock".into(),
            transport: Default::default(),
            command: "python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            env: Default::default(),
            url: None,
            headers: Default::default(),
            enabled: true,
        })
        .unwrap();
    let session = engine
        .create_session(project.id, "dynamic mcp".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..150 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals.iter().filter(|a| a.status == "pending") {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    set_approval_mode(&service, ApprovalMode::Strict);
    service.start(&session.id, "use dynamic mcp").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "mcp_mock_echo"
            && event.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("dynamic mcp"))
    }));
    assert!(events.iter().any(|event| {
        event.kind == "approval.requested"
            && event.payload["tool"] == "mcp_mock_echo"
            && event.payload["approval_risk"] == "external"
    }));
}

#[tokio::test]
async fn remembers_and_recalls_project_memory_from_tools() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "remember_1".into(),
                    name: "remember".into(),
                    arguments: serde_json::json!({
                        "kind": "decision",
                        "title": "Keep merges deterministic",
                        "body": "Do not use an LLM in the Integrator.",
                        "tags": ["architecture"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "recall_1".into(),
                    name: "recall".into(),
                    arguments: serde_json::json!({"query": "deterministic"}).to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("memory done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "memory tools".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "remember a decision")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let memories = engine
        .list_memories(&project.id, None, None, 10)
        .await
        .unwrap()
        .memories;
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].title, "Keep merges deterministic");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "recall"
            && event.payload["output"]
                .as_str()
                .is_some_and(|output| output.contains("Keep merges deterministic"))
    }));
}

#[tokio::test]
async fn probes_an_openai_compatible_endpoint_over_http() {
    use vp_providers::OpenAiCompatibleProvider;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .mount(&server)
        .await;
    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatibleProvider::local(
        "openai",
        format!("{}/v1", server.uri()),
    ));
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service_with_provider(&dir, provider);
    let result = service.probe().await.unwrap();
    assert_eq!(result.text, "OK");
    assert_eq!(result.provider, "queue");
    assert_eq!(result.model, "test");
}

#[tokio::test]
async fn global_capacity_serializes_worker_slots() {
    let capacity = Arc::new(CapacityController::new(1, 1));
    assert_eq!(
        capacity.snapshot(),
        CapacityInfo {
            worker_limit: 1,
            worker_available: 1,
            labour_limit: 1,
            labour_available: 1,
        }
    );
    let probe = capacity
        .try_acquire(AgentRole::Worker, "session-a")
        .unwrap();
    drop(probe);
    let control = RunControl::new();
    let first = capacity
        .acquire(AgentRole::Worker, "session-a", 0, &control)
        .await
        .unwrap();
    assert_eq!(capacity.snapshot().worker_available, 0);
    assert!(
        capacity
            .try_acquire(AgentRole::Worker, "session-b")
            .is_none()
    );
    let second_capacity = capacity.clone();
    let second_control = Arc::new(RunControl::new());
    let mut waiter = tokio::spawn(async move {
        second_capacity
            .acquire(AgentRole::Worker, "session-b", 0, &second_control)
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(120), &mut waiter)
            .await
            .is_err()
    );
    drop(first);
    let acquired = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(acquired.is_some());
    assert_eq!(capacity.snapshot().worker_available, 0);

    let third_control = Arc::new(RunControl::new());
    let third_capacity = capacity.clone();
    let worker_control = third_control.clone();
    let cancelled = tokio::spawn(async move {
        third_capacity
            .acquire(AgentRole::Worker, "session-c", 0, &worker_control)
            .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    third_control.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), cancelled)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn capacity_round_robins_across_sessions() {
    let capacity = Arc::new(CapacityController::new(1, 1));
    let first = capacity
        .try_acquire(AgentRole::Worker, "session-a")
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let a_capacity = capacity.clone();
    let a_control = Arc::new(RunControl::new());
    let a_sender = sender.clone();
    let a = tokio::spawn(async move {
        let permit = a_capacity
            .acquire(AgentRole::Worker, "session-a", 0, &a_control)
            .await
            .unwrap();
        a_sender.send("a2").unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let b_capacity = capacity.clone();
    let b_control = Arc::new(RunControl::new());
    let b_sender = sender.clone();
    let b = tokio::spawn(async move {
        let permit = b_capacity
            .acquire(AgentRole::Worker, "session-b", 0, &b_control)
            .await
            .unwrap();
        b_sender.send("b1").unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(first);
    let first_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let second_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first_grant, second_grant), ("b1", "a2"));
    a.await.unwrap();
    b.await.unwrap();
}

#[tokio::test]
async fn capacity_prefers_higher_priority_tasks() {
    let capacity = Arc::new(CapacityController::new(1, 1));
    let first = capacity
        .try_acquire(AgentRole::Worker, "session-holder")
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let low_capacity = capacity.clone();
    let low_control = Arc::new(RunControl::new());
    let low_sender = sender.clone();
    let low = tokio::spawn(async move {
        let permit = low_capacity
            .acquire(AgentRole::Worker, "session-low", -10, &low_control)
            .await
            .unwrap();
        low_sender.send("low").unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    let high_capacity = capacity.clone();
    let high_control = Arc::new(RunControl::new());
    let high_sender = sender.clone();
    let high = tokio::spawn(async move {
        let permit = high_capacity
            .acquire(AgentRole::Worker, "session-high", 50, &high_control)
            .await
            .unwrap();
        high_sender.send("high").unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(first);

    let first_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let second_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first_grant, second_grant), ("high", "low"));
    low.await.unwrap();
    high.await.unwrap();
}

#[tokio::test]
async fn admission_prefers_higher_priority_sessions() {
    let gate = Arc::new(AdmissionGate::new(1));
    let holder_control = Arc::new(RunControl::new());
    let holder = gate
        .acquire("session-holder", "project-holder", 0, &holder_control)
        .await
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let low_gate = gate.clone();
    let low_control = Arc::new(RunControl::new());
    let low_sender = sender.clone();
    let low = tokio::spawn(async move {
        let permit = low_gate
            .acquire("session-low", "project-low", -10, &low_control)
            .await
            .unwrap();
        low_sender.send("low").unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    let high_gate = gate.clone();
    let high_control = Arc::new(RunControl::new());
    let high_sender = sender.clone();
    let high = tokio::spawn(async move {
        let permit = high_gate
            .acquire("session-high", "project-high", 50, &high_control)
            .await
            .unwrap();
        high_sender.send("high").unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(gate.snapshot(), (1, 1, 2));
    drop(holder);

    let first_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let second_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first_grant, second_grant), ("high", "low"));
    low.await.unwrap();
    high.await.unwrap();
}

#[tokio::test]
async fn capacity_aging_boosts_low_priority_waiters() {
    let capacity = Arc::new(CapacityController::new_with_aging(
        1,
        1,
        Duration::from_millis(10),
    ));
    let holder = capacity
        .try_acquire(AgentRole::Worker, "session-holder")
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let low_capacity = capacity.clone();
    let low_control = Arc::new(RunControl::new());
    let low_sender = sender.clone();
    let low = tokio::spawn(async move {
        let permit = low_capacity
            .acquire(AgentRole::Worker, "session-low", 0, &low_control)
            .await
            .unwrap();
        low_sender.send("low").unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let high_capacity = capacity.clone();
    let high_control = Arc::new(RunControl::new());
    let high_sender = sender.clone();
    let high = tokio::spawn(async move {
        let permit = high_capacity
            .acquire(AgentRole::Worker, "session-high", 5, &high_control)
            .await
            .unwrap();
        high_sender.send("high").unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(holder);

    let first_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let second_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first_grant, second_grant), ("low", "high"));
    low.await.unwrap();
    high.await.unwrap();
}

#[tokio::test]
async fn admission_aging_boosts_low_priority_sessions() {
    let gate = Arc::new(AdmissionGate::new_with_aging(1, Duration::from_millis(10)));
    let holder_control = Arc::new(RunControl::new());
    let holder = gate
        .acquire("session-holder", "project-holder", 0, &holder_control)
        .await
        .unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();

    let low_gate = gate.clone();
    let low_control = Arc::new(RunControl::new());
    let low_sender = sender.clone();
    let low = tokio::spawn(async move {
        let permit = low_gate
            .acquire("session-low", "project-low", 0, &low_control)
            .await
            .unwrap();
        low_sender.send("low").unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let high_gate = gate.clone();
    let high_control = Arc::new(RunControl::new());
    let high_sender = sender.clone();
    let high = tokio::spawn(async move {
        let permit = high_gate
            .acquire("session-high", "project-high", 5, &high_control)
            .await
            .unwrap();
        high_sender.send("high").unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(permit);
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(holder);

    let first_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let second_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first_grant, second_grant), ("low", "high"));
    low.await.unwrap();
    high.await.unwrap();
}

#[tokio::test]
async fn scope_locks_serialize_overlapping_sessions() {
    let locks = Arc::new(ScopeLocks::default());
    let control_a = RunControl::new();
    let guard_a = locks
        .acquire(
            "project",
            "session-a",
            "agent-a",
            &["src/**".into()],
            &control_a,
        )
        .await
        .unwrap()
        .unwrap();
    let same_session = locks
        .acquire(
            "project",
            "session-a",
            "agent-b",
            &["src/api/**".into()],
            &control_a,
        )
        .await
        .unwrap();
    assert!(same_session.is_some());
    drop(same_session);

    let locks_b = locks.clone();
    let control_b = RunControl::new();
    let mut waiter = tokio::spawn(async move {
        locks_b
            .acquire(
                "project",
                "session-b",
                "agent-c",
                &["src/api/**".into()],
                &control_b,
            )
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(120), &mut waiter)
            .await
            .is_err()
    );
    drop(guard_a);
    let acquired = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(acquired.is_some());
}

#[tokio::test]
async fn persistent_scope_locks_publish_store_leases() {
    let dir = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("leases"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    let locks = Arc::new(ScopeLocks::with_store(store.clone()));
    let control = RunControl::new();
    let guard = locks
        .acquire(
            &project.id,
            &first.id,
            "agent-a",
            &["src/api/**".into()],
            &control,
        )
        .await
        .unwrap()
        .unwrap();
    let leases = store.list_write_leases(Some(&project.id), None).unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].agent_id, "agent-a");
    assert_eq!(leases[0].scopes, vec!["src/api".to_string()]);

    let waiting_locks = locks.clone();
    let waiting_control = RunControl::new();
    let second_id = second.id.clone();
    let project_id = project.id.clone();
    let mut waiter = tokio::spawn(async move {
        waiting_locks
            .acquire(
                &project_id,
                &second_id,
                "agent-b",
                &["src/api/handler".into()],
                &waiting_control,
            )
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(120), &mut waiter)
            .await
            .is_err()
    );
    assert!(store.list_write_leases(None, None).unwrap().len() == 1);
    drop(guard);
    let second_guard = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .list_write_leases(Some(&project.id), Some(&second.id))
            .unwrap()[0]
            .agent_id,
        "agent-b"
    );
    drop(second_guard);
    assert!(store.list_write_leases(None, None).unwrap().is_empty());
}

struct BlockingProvider {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Provider for BlockingProvider {
    fn id(&self) -> &str {
        "blocking"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::Done),
        ])))
    }
}

#[tokio::test]
async fn pauses_and_resumes_one_running_plan_task() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(BlockingProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id.clone(), "pause one task".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "pause one task".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![PlanTask {
            id: "T1".into(),
            department: "impl".into(),
            title: "One".into(),
            kind: "coding".into(),
            priority: 0,
            profile: None,
            deps: Vec::new(),
            write_scope: vec!["worker.txt".into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    assert!(service.resume_plan(&session.id).await.unwrap());
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    wait_for_task_status(&engine, &session, "T1", "running").await;
    let pausing = engine.pause_plan_task(&session.id, "T1").await.unwrap();
    assert_eq!(pausing.status, "pausing");
    provider.release.notify_one();
    wait_for_task_status(&engine, &session, "T1", "paused").await;
    let sessions = engine
        .list_sessions(Some(project.id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "running");
    let resumed = engine.resume_plan_task(&session.id, "T1").await.unwrap();
    assert_eq!(resumed.status, "running");
    let session = wait_until_idle(&engine, &session).await;
    assert_eq!(session.status, "idle");
    let queue = engine
        .task_queue(Some(project.id.clone()), Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(queue.tasks[0].status, "completed");
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.task_queue_matches, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

async fn wait_for_task_status(engine: &Engine, session: &Session, task_id: &str, expected: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let queue = engine
            .task_queue(Some(session.project_id.clone()), Some(session.id.clone()))
            .await
            .unwrap();
        if queue
            .tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .is_some_and(|task| task.status == expected)
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "task {task_id} did not reach {expected}: {:?}",
            queue.tasks
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn cancels_one_running_plan_task_without_cancelling_the_session() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(BlockingProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id.clone(), "cancel one task".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "cancel one task".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![PlanTask {
            id: "T1".into(),
            department: "impl".into(),
            title: "One".into(),
            kind: "coding".into(),
            priority: 0,
            profile: None,
            deps: Vec::new(),
            write_scope: vec!["worker.txt".into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    assert!(service.resume_plan(&session.id).await.unwrap());
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let queue = engine
            .task_queue(Some(project.id.clone()), Some(session.id.clone()))
            .await
            .unwrap();
        if queue.tasks[0].status == "running" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "task did not start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cancelling = engine
        .request_plan_task_cancel(&session.id, "T1")
        .await
        .unwrap();
    assert_eq!(cancelling.status, "cancelling");
    provider.release.notify_one();
    let session = wait_until_idle(&engine, &session).await;
    assert_eq!(session.status, "idle");
    let queue = engine
        .task_queue(Some(project.id.clone()), Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(queue.tasks[0].status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "plan_task.finished"
            && event.payload["task_id"] == "T1"
            && event.payload["task_status"] == "cancelled"
    }));
}

#[tokio::test]
async fn global_session_cap_queues_second_run() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(BlockingProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (engine, service, project) = test_service_with_provider_cap(&dir, provider.clone(), 1);
    let first = engine
        .create_session(project.id.clone(), "first".into(), None)
        .await
        .unwrap();
    let second = engine
        .create_session(project.id.clone(), "second".into(), None)
        .await
        .unwrap();
    engine.create_leader_agent(&first.id, "test").await.unwrap();
    engine
        .create_leader_agent(&second.id, "test")
        .await
        .unwrap();
    service.start(&first.id, "first").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    service.start(&second.id, "second").await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    let sessions = engine
        .list_sessions(Some(project.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == first.id)
            .unwrap()
            .status,
        "running"
    );
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == second.id)
            .unwrap()
            .status,
        "queued"
    );
    provider.release.notify_one();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let sessions = engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap();
        if sessions.iter().all(|session| session.status == "idle") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queued session did not run: {:?}",
            sessions
                .iter()
                .map(|session| (&session.id, &session.status))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn pauses_and_resumes_between_turns() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(PausingProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "pause".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "pause").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    assert!(service.pause(&session.id).await.unwrap());
    provider.release.notify_one();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "paused");
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(service.resume(&session.id).await.unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        if sessions[0].status == "idle" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session did not resume: {}",
            sessions[0].status
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "agent.paused"));
    assert!(events.iter().any(|event| event.kind == "agent.resumed"));
    assert!(!service.pause(&session.id).await.unwrap());
}

#[tokio::test]
async fn fails_a_run_when_the_provider_stream_is_idle() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_provider_idle_timeout(&service, 1);
    let session = engine
        .create_session(project.id, "stall".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "stall").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "failed");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.failed"
            && event.payload["error"]
                .as_str()
                .is_some_and(|error| error.contains("idle for 1s"))
    }));
}

#[tokio::test]
async fn cancels_a_slow_provider_stream() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "cancel me".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "cancel me").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(service.cancel(&session.id));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "agent.cancelled"));
    let sessions = engine
        .list_sessions(Some(session.project_id))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "cancelled");
    assert!(!service.cancel(&session.id));
}

#[test]
fn task_priority_contributes_to_capacity_priority() {
    assert_eq!(task_capacity_priority(0, 20), 20);
    assert_eq!(task_capacity_priority(10, -20), -10);
    assert_eq!(task_capacity_priority(100, 100), 100);
    assert_eq!(task_capacity_priority(-100, -100), -100);
}

#[test]
fn worker_auto_expansion_respects_budget_and_hard_cap() {
    let caps = AgentCaps {
        managers: 5,
        workers: 2,
        labour: 10,
    };
    let budget = SessionBudget::new(Some(1_000), 100, caps);
    assert_eq!(budget.worker_cap(), 2);
    assert!(budget.can_expand_workers());
    budget.expand_workers(8);
    assert_eq!(budget.worker_cap(), 8);
    assert_eq!(budget.available(AgentRole::Worker), 8);
    budget.expand_workers(99);
    assert_eq!(budget.worker_cap(), 10);

    let tight = SessionBudget::new(Some(1_000), 900, caps);
    assert!(!tight.can_expand_workers());
}

fn write_acp_agent(path: &Path) {
    std::fs::write(
        path,
        r#"#!/usr/bin/env python3
import json, os, sys

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

def reply(req, result):
    send({"jsonrpc": "2.0", "id": req.get("id"), "result": result})

for line in sys.stdin:
    if not line.strip():
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        reply(req, {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "authMethods": [],
            "agentInfo": {"name": "fake-acp", "version": "0.0.1"},
        })
    elif method == "session/new":
        reply(req, {"sessionId": "fake-session"})
    elif method == "session/prompt":
        send({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "fake-session",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "writing acp.txt "}},
        }})
        send({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission", "params": {
            "sessionId": "fake-session",
            "toolCall": {"toolCallId": "tool-1", "title": "Write acp.txt", "kind": "edit", "rawInput": {"path": "acp.txt"}},
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "deny", "name": "Reject", "kind": "reject_once"},
            ],
        }})
        while True:
            response = json.loads(sys.stdin.readline())
            if response.get("id") == "perm-1":
                outcome = (response.get("result") or {}).get("outcome") or {}
                if outcome.get("outcome") != "selected" or outcome.get("optionId") != "allow":
                    reply(req, {"stopReason": "refusal"})
                    break
                with open(os.path.join(os.getcwd(), "acp.txt"), "w") as handle:
                    handle.write("from acp\n")
                send({"jsonrpc": "2.0", "method": "session/update", "params": {
                    "sessionId": "fake-session",
                    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "done"}},
                }})
                reply(req, {"stopReason": "end_turn"})
                break
        else:
            continue
        continue
    elif method == "session/cancel":
        pass
    else:
        send({"jsonrpc": "2.0", "id": req.get("id"), "error": {"code": -32601, "message": "not found"}})
"#,
    )
    .unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn delegates_to_an_external_acp_agent_and_integrates_it() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("fake-acp-agent.py");
    write_acp_agent(&script);
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Implementation",
                        "objective": "use the external agent"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_acp".into(),
                    name: "delegate_acp".into(),
                    arguments: serde_json::json!({
                        "agent": "fake-acp",
                        "objective": "write acp.txt",
                        "write_scope": ["acp.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "acp.txt exists with the expected content"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .add_acp_agent(
            "fake-acp",
            "/usr/bin/python3",
            vec![script.to_string_lossy().to_string()],
            std::collections::BTreeMap::new(),
            true,
            false,
            120,
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "external".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..300 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals
                    .iter()
                    .filter(|approval| approval.status == "pending")
                {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    set_approval_mode(&service, ApprovalMode::Strict);
    let worktree = session.worktree.clone().unwrap();
    service.start(&session.id, "external").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if std::fs::read_to_string(std::path::Path::new(&worktree).join("acp.txt"))
            .ok()
            .as_deref()
            == Some("from acp\n")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "external ACP changes were not integrated"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&worktree).join("acp.txt")).unwrap(),
        "from acp\n"
    );
    let agents = engine.list_agents(&session.id).await.unwrap();
    let external = agents
        .iter()
        .find(|agent| agent.model == "acp:fake-acp")
        .expect("external ACP worker is recorded as a worker");
    assert_eq!(external.role, "worker");
    assert_eq!(external.status, "idle");
    assert_eq!(external.title.as_deref(), Some("ACP · fake-acp"));
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "merged");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| { event.kind == "agent.spawned" && event.payload["backend"] == "acp" })
    );
    assert!(events.iter().any(|event| {
        event.kind == "approval.requested"
            && event.payload["tool"]
                .as_str()
                .is_some_and(|tool| tool.starts_with("acp."))
            && event.payload["approval_risk"] == "workspace_write"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "worker"
            && event.payload["merged"] == true
    }));
}

#[tokio::test]
async fn retries_a_stream_failure_before_output() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![Err(ProviderError::Stream("connection reset".into()))],
            vec![
                Ok(StreamEvent::TextDelta("recovered".into())),
                Ok(StreamEvent::Done),
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "retry stream".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "retry stream").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.stream_retry"
            && event.payload["attempt"] == 1
            && event.payload["phase"] == "stream"
    }));
    assert!(
        events
            .iter()
            .any(|event| { event.kind == "agent.message" && event.payload["text"] == "recovered" })
    );
}

#[tokio::test]
async fn preserves_partial_output_and_usage_when_the_stream_breaks() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            Ok(StreamEvent::TextDelta("half a thought".into())),
            Err(ProviderError::Stream("connection reset".into())),
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "partial".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "partial").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "failed");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.message"
            && event.payload["text"] == "half a thought"
            && event.payload["partial"] == true
    }));
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "agent.stream_retry"),
        "partial output must not be replayed"
    );
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 1);
    assert!(ledger.entries[0].estimated);
    assert!(ledger.entries[0].input_tokens > 0);
    assert!(ledger.entries[0].output_tokens > 0);
    assert_eq!(ledger.totals.input_tokens, ledger.entries[0].input_tokens);
    assert!(events.iter().any(|event| {
        event.kind == "agent.failed"
            && event.payload["error"]
                .as_str()
                .is_some_and(|error| error.contains("connection reset"))
    }));
}

#[tokio::test]
async fn records_partial_usage_when_a_stream_is_cancelled() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(PartialSlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "cancel partial".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "cancel partial").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine.list_events(session.id.clone(), 0, 50).await.unwrap();
        if events
            .iter()
            .any(|event| event.kind == "agent.message_delta" && event.payload["text"] == "partial")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "partial output was not streamed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(service.cancel(&session.id));
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "cancelled");
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 1);
    assert!(ledger.entries[0].estimated);
    assert!(ledger.entries[0].output_tokens > 0);
}

#[tokio::test]
async fn handles_a_long_provider_stream_without_losing_chunks() {
    let dir = TempDir::new().unwrap();
    let expected = (0..500)
        .map(|index| format!("{index},"))
        .collect::<String>();
    let mut events = (0..500)
        .map(|index| Ok(StreamEvent::TextDelta(format!("{index},"))))
        .collect::<Vec<_>>();
    events.push(Ok(StreamEvent::Done));
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![events])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "long stream".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "long stream").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 2_000)
        .await
        .unwrap();
    let message = events
        .iter()
        .find(|event| event.kind == "agent.message")
        .expect("final assistant message");
    assert_eq!(message.payload["text"].as_str().unwrap(), expected);
    let messages = engine.list_messages(&session.id).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == "assistant" && message.content == expected)
    );
}

#[tokio::test]
async fn dispatches_one_ready_plan_task_per_call() {
    let dir = TempDir::new().unwrap();
    fn write_task(path: &str, content: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCall(ToolCall {
                id: format!("write_{path}"),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": path, "content": content}).to_string(),
            }),
            StreamEvent::Done,
        ]
    }
    fn verdict(task: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCall(ToolCall {
                id: format!("verdict_{task}"),
                name: "submit_verdict".into(),
                arguments: serde_json::json!({
                    "approved": true,
                    "summary": format!("{task} matches the task")
                })
                .to_string(),
            }),
            StreamEvent::Done,
        ]
    }
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            write_task("src/t1.txt", "one\n"),
            vec![StreamEvent::TextDelta("T1 done".into()), StreamEvent::Done],
            verdict("T1"),
            write_task("src/t2.txt", "two\n"),
            vec![StreamEvent::TextDelta("T2 done".into()), StreamEvent::Done],
            verdict("T2"),
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "dispatch tasks".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "dispatch tasks".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![
            PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "One".into(),
                kind: "coding".into(),
                priority: 10,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/t1.txt".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
            PlanTask {
                id: "T2".into(),
                department: "impl".into(),
                title: "Two".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/t2.txt".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
        ],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();

    let approver_engine = engine.clone();
    let approver_session = session.id.clone();
    let approver = tokio::spawn(async move {
        for _ in 0..600 {
            if let Ok(approvals) = approver_engine
                .list_approvals(Some(approver_session.clone()))
                .await
            {
                for approval in approvals
                    .iter()
                    .filter(|approval| approval.status == "pending")
                {
                    let _ = approver_engine.resolve_approval(&approval.id, true).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    async fn wait_for_task(engine: &Engine, session: &Session, task_id: &str, status: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let events = engine
                .list_events(session.id.clone(), 0, 500)
                .await
                .unwrap();
            if events.iter().any(|event| {
                event.kind == "plan_task.finished"
                    && event.payload["task_id"] == task_id
                    && event.payload["task_status"] == status
            }) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "task {task_id} did not finish with {status}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    assert!(service.dispatch_next_task(&session.id).await.unwrap());
    wait_for_task(&engine, &session, "T1", "completed").await;
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let queue = engine
        .task_queue(Some(project.id.clone()), Some(session.id.clone()))
        .await
        .unwrap();
    let t1 = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T1")
        .unwrap();
    let t2 = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T2")
        .unwrap();
    assert_eq!(t1.status, "completed");
    assert_eq!(t2.status, "pending");
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "plan_task.started")
            .count(),
        1,
        "only one task should be dispatched per call"
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.task_dispatched" && event.payload["task_limit"] == 1
    }));

    assert!(service.dispatch_next_task(&session.id).await.unwrap());
    wait_for_task(&engine, &session, "T2", "completed").await;
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let queue = engine
        .task_queue(Some(project.id), Some(session.id.clone()))
        .await
        .unwrap();
    assert!(queue.tasks.iter().all(|task| task.status == "completed"));
    approver.abort();
}

#[tokio::test]
async fn cancelling_a_dispatched_task_does_not_cancel_the_rest_of_the_plan() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "cancel dispatch".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "cancel dispatch".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![
            PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "One".into(),
                kind: "coding".into(),
                priority: 10,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/t1".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
            PlanTask {
                id: "T2".into(),
                department: "impl".into(),
                title: "Two".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/t2".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
        ],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();

    assert!(service.dispatch_next_task(&session.id).await.unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        if events
            .iter()
            .any(|event| event.kind == "plan_task.started" && event.payload["task_id"] == "T1")
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "T1 did not start");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(service.cancel(&session.id));
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "cancelled");
    let queue = engine
        .task_queue(Some(project.id), Some(session.id.clone()))
        .await
        .unwrap();
    let t1 = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T1")
        .unwrap();
    let t2 = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T2")
        .unwrap();
    assert_eq!(t1.status, "cancelled");
    assert_eq!(
        t2.status, "pending",
        "limited dispatch must not cancel tasks it did not claim"
    );
}

#[tokio::test]
async fn compacts_long_conversations_and_resumes_from_the_summary() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta(
                    "The user investigated the release pipeline. Files a.rs and b.rs were inspected; tests passed."
                        .into(),
                ),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("compaction complete".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 50, 4);
    let session = engine
        .create_session(project.id, "long context".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..20 {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        engine
            .append_message(
                &session.id,
                role,
                &format!(
                    "message {index}: the release pipeline investigation includes enough detail to exceed the tiny test threshold"
                ),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }

    service
        .start(&session.id, "current prompt after compaction")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let compacted = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted event");
    assert!(
        compacted.payload["threshold_tokens"]
            .as_u64()
            .is_some_and(|threshold| threshold >= 50)
    );
    assert!(
        compacted.payload["summarized_messages"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        compacted.payload["through_message_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        compacted.payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.starts_with("Earlier conversation summary"))
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "context.compact_failed")
    );
    let usage_event = events
        .iter()
        .find(|event| event.kind == "context.usage")
        .expect("context.usage event");
    assert!(usage_event.payload["estimated_tokens"].as_u64().is_some());
    assert!(
        usage_event.payload["estimated_tokens"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= usage_event.payload["hard_limit_tokens"]
                .as_u64()
                .unwrap_or(0)
    );
    let status = engine.session_context_status(&session.id).await.unwrap();
    assert!(status.usage.is_some());
    assert_eq!(
        status
            .last_compaction
            .as_ref()
            .map(|summary| summary.mode.as_str()),
        Some("summary")
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "compaction complete"
    }));

    let stored = engine.list_messages(&session.id).await.unwrap();
    assert_eq!(
        stored
            .iter()
            .filter(|message| message.content == "message 0: the release pipeline investigation includes enough detail to exceed the tiny test threshold")
            .count(),
        1,
        "pre-compaction history must remain stored"
    );
    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert!(
        loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
    assert!(
        loaded
            .messages
            .iter()
            .any(|message| message.content == "current prompt after compaction")
    );
    assert!(
        loaded.messages.len() < stored.len() + 1,
        "active conversation should be smaller than the stored history"
    );
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert!(
        ledger
            .entries
            .iter()
            .any(|entry| entry.role == "compaction")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.consistent, "compaction replay drifted: {replay:?}");
}

#[tokio::test]
async fn manually_compacts_an_idle_session_without_starting_a_prompt() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta(
                "Manual summary: the user investigated the release pipeline and tests passed."
                    .into(),
            ),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 0, 4);
    let session = engine
        .create_session(project.id.clone(), "manual context".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..12 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!(
                    "message {index}: manual compaction should summarize this history even below the automatic threshold"
                ),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }

    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);
    let compaction = result.compaction.expect("compaction summary");
    assert_eq!(compaction.mode, "summary");
    assert!(compaction.summarized_messages > 0);
    assert!(compaction.estimated_tokens_after < compaction.estimated_tokens_before);

    // Manual compaction must not start a prompt or change the session status.
    let sessions = engine.list_sessions(Some(project.id)).await.unwrap();
    assert_eq!(sessions[0].status, "draft");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "context.compacted" && event.payload["mode"] == "summary")
    );
    assert!(!events.iter().any(|event| event.kind == "agent.message"));
    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert!(
        loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
    let status = engine.session_context_status(&session.id).await.unwrap();
    let usage = status.usage.expect("post-compaction usage");
    let summary = status.last_compaction.expect("last compaction");
    assert_eq!(usage.estimated_tokens, summary.estimated_tokens_after);
    assert!(usage.estimated_tokens < summary.estimated_tokens_before);
}

#[tokio::test]
async fn manual_compaction_refuses_an_active_session() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "active context".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let error = service.compact_now(&session.id).await.unwrap_err();
    assert!(error.to_string().contains("must not be queued"));
}

#[tokio::test]
async fn manual_compaction_surfaces_failure_and_releases_the_session() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![Err(ProviderError::Stream("summary unavailable".into()))],
            vec![
                Ok(StreamEvent::TextDelta("recovered summary".into())),
                Ok(StreamEvent::Done),
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 0, 4);
    let session = engine
        .create_session(project.id, "manual failure".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..12 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: manual failure should release the run registry"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }

    let error = service.compact_now(&session.id).await.unwrap_err();
    assert!(error.to_string().contains("context compaction failed"));
    assert!(error.to_string().contains("summary unavailable"));
    // The failed run must have released the registry, so a second manual
    // compaction can proceed instead of reporting "already running".
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);
    assert_eq!(
        result.compaction.map(|summary| summary.mode),
        Some("summary".into())
    );
}

#[tokio::test]
async fn manual_compaction_cannot_be_paused() {
    let dir = TempDir::new().unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider: Arc<dyn Provider> = Arc::new(GatedSummaryProvider {
        started: started.clone(),
        release: release.clone(),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let service = Arc::new(service);
    set_compaction(&service, 0, 4);
    let session = engine
        .create_session(project.id.clone(), "pausable context".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..12 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: manual compaction must not be pausable"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }

    let session_id = session.id.clone();
    let compact = tokio::spawn({
        let service = service.clone();
        async move { service.compact_now(&session_id).await }
    });
    started.notified().await;
    assert!(!service.pause(&session.id).await.unwrap());
    release.notify_one();
    let result = compact.await.unwrap().unwrap();
    assert!(result.compacted);
    let sessions = engine.list_sessions(Some(project.id)).await.unwrap();
    assert_eq!(sessions[0].status, "draft");
}

#[tokio::test]
async fn compaction_failure_does_not_abort_the_run() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![Err(ProviderError::Stream("compaction unavailable".into()))],
            vec![
                Ok(StreamEvent::TextDelta("normal answer".into())),
                Ok(StreamEvent::Done),
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 20, 3);
    let session = engine
        .create_session(project.id, "failed compaction".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..24 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!(
                    "message {index}: enough context to trigger the tiny compaction threshold"
                ),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    service
        .start(&session.id, "continue without compaction")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();

    assert!(
        events
            .iter()
            .any(|event| event.kind == "context.compact_failed")
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "normal answer"
    }));
}

#[test]
fn derives_context_limits_from_the_configured_model_window() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();

    config.context_window = 128_000;
    let (threshold_128k, hard_128k) = context_limits(&config, AgentRole::Leader, 2_000, 2_000);
    assert!(threshold_128k > 80_000, "128k model threshold too low");
    assert!(hard_128k > threshold_128k);

    config.context_window = 1_000_000;
    let (threshold_1m, hard_1m) = context_limits(&config, AgentRole::Leader, 2_000, 2_000);
    assert!(threshold_1m > 700_000, "1M model threshold too low");
    assert!(hard_1m > threshold_1m);

    config.context_window = 8_000;
    config.max_tokens = Some(2_048);
    let (threshold_8k, hard_8k) = context_limits(&config, AgentRole::Leader, 1_000, 1_000);
    assert!(threshold_8k < 4_000, "8k model threshold too high");
    assert!(hard_8k < 5_000);

    config.context_window = 8_000;
    config.role_models.worker.context_window = Some(200_000);
    assert!(
        context_window_for(&config, AgentRole::Worker)
            > context_window_for(&config, AgentRole::Leader)
    );

    config.context_window = 0;
    let (threshold_off, hard_off) = context_limits(&config, AgentRole::Leader, 0, 0);
    assert_eq!(threshold_off, 0);
    assert_eq!(hard_off, u64::MAX);
}

#[tokio::test]
async fn trims_older_messages_when_summarization_fails() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![Err(ProviderError::Stream(
            "summary unavailable".into(),
        ))]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "trim context".into(), None)
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_keep_messages = 2;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),
    };
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 1,
            labour: 1,
        },
    );
    let control = RunControl::new();
    let mut conversation = Vec::new();
    let mut conversation_ids = Vec::new();
    for index in 0..10 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                "this message contains enough text to make the context exceed the tiny hard limit",
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
        conversation.push(ChatMessage {
            role: message.role,
            content: message.content,
            tool_calls: None,
            tool_call_id: None,
        });
        conversation_ids.push(Some(message.id));
    }

    let changed = compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        10,
        50,
        0,
        1.0,
        false,
    )
    .await
    .unwrap();
    assert!(changed);
    assert!(conversation.len() < 10);
    assert_eq!(conversation.len(), conversation_ids.len());
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "context.compact_failed")
    );
    let compacted = events
        .iter()
        .find(|event| event.kind == "context.compacted" && event.payload["mode"] == "trim")
        .expect("trim event");
    assert!(
        compacted.payload["dropped_messages"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert!(loaded.messages.len() < 10);
    assert!(
        !loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
}

#[tokio::test]
async fn compaction_counts_system_and_tool_tokens() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "active context".into(), None)
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_keep_messages = 2;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),
    };
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 1,
            labour: 1,
        },
    );
    let control = RunControl::new();
    let mut conversation = Vec::new();
    let mut conversation_ids = Vec::new();
    for index in 0..6 {
        conversation.push(ChatMessage::user("context payload"));
        conversation_ids.push(Some(format!("message-{index}")));
    }
    let message_tokens = estimate_messages_tokens(&conversation);
    assert!(
        message_tokens < 50,
        "messages alone must stay under the limit"
    );

    // System/tool tokens belong to the active request. With 40 base tokens the
    // same conversation is over the 50-token hard limit and must be trimmed.
    let changed = compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        0,
        50,
        40,
        1.0,
        false,
    )
    .await
    .unwrap();
    assert!(changed);
    assert!(conversation.len() < 6);
    assert_eq!(conversation.len(), conversation_ids.len());
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let compacted = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("trim event");
    assert_eq!(compacted.payload["mode"], "trim");
    let before = compacted.payload["estimated_tokens_before"]
        .as_u64()
        .unwrap();
    let after = compacted.payload["estimated_tokens_after"]
        .as_u64()
        .unwrap();
    assert_eq!(before, message_tokens + 40);
    assert!(after < before);
    assert!(after >= 40);
}

#[test]
fn truncate_does_not_split_utf8_characters() {
    let value = "界".repeat(3_000);
    let truncated = truncate(&value);
    assert!(truncated.ends_with("… (truncated)"));
    assert!(truncated.chars().count() < value.chars().count());
    let value = "a".repeat(8_000);
    assert_eq!(truncate(&value), value);
}

#[tokio::test]
async fn compaction_applies_context_calibration() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "calibrated context".into(), None)
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_keep_messages = 2;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),
    };
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 1,
            labour: 1,
        },
    );
    let control = RunControl::new();
    let mut conversation = Vec::new();
    let mut conversation_ids = Vec::new();
    for index in 0..6 {
        conversation.push(ChatMessage::user("context payload"));
        conversation_ids.push(Some(format!("message-{index}")));
    }
    let raw = estimate_messages_tokens(&conversation).saturating_add(40);
    assert!(raw < 100, "raw estimate should fit the hard limit");

    let changed = compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        0,
        100,
        40,
        2.0,
        false,
    )
    .await
    .unwrap();
    assert!(changed);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let compacted = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("calibrated trim event");
    assert_eq!(
        compacted.payload["estimated_tokens_before"].as_u64(),
        Some(calibrate_context_tokens(raw, 2.0))
    );
    assert_eq!(compacted.payload["calibration"].as_f64(), Some(2.0));
    assert!(
        compacted.payload["estimated_tokens_after"]
            .as_u64()
            .is_some_and(|tokens| tokens < calibrate_context_tokens(raw, 2.0))
    );
}

#[test]
fn context_calibration_only_scales_up_and_caps() {
    let mut calibration = ContextCalibration::default();
    calibration.observe(1_000, 800);
    assert_eq!(calibration.factor, 1.0);
    assert_eq!(calibration.samples, 1);

    calibration.observe(1_000, 1_500);
    assert!(calibration.factor > 1.0);
    for _ in 0..20 {
        calibration.observe(1_000, 4_000);
    }
    assert_eq!(calibration.factor, 2.0);

    assert_eq!(calibrate_context_tokens(1_000, 1.0), 1_000);
    assert_eq!(calibrate_context_tokens(1_000, 0.5), 1_000);
    assert_eq!(calibrate_context_tokens(1_000, 2.0), 2_000);
    assert_eq!(calibrate_context_tokens(1_000, f64::NAN), 1_000);
}

#[tokio::test]
async fn provider_usage_calibrates_future_context_estimates() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("first answer".into()),
                StreamEvent::Usage {
                    input_tokens: 20_000,
                    output_tokens: 10,
                },
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second answer".into()),
                StreamEvent::Usage {
                    input_tokens: 20_000,
                    output_tokens: 10,
                },
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.context_window = 200_000;
        config.compact_keep_messages = 4;
        *service.config.write().unwrap() = Some(config);
    }
    let session = engine
        .create_session(project.id, "measure context".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    service.start(&session.id, "measure this").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let measured = events
        .iter()
        .find(|event| event.kind == "context.measured")
        .expect("context.measured event");
    assert_eq!(measured.payload["actual_input_tokens"], 20_000);
    assert_eq!(measured.payload["actual_output_tokens"], 10);
    assert!(
        measured.payload["estimated_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(
        measured.payload["calibration"]
            .as_f64()
            .is_some_and(|calibration| calibration > 1.0)
    );

    service.start(&session.id, "measure again").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "context.usage"
            && event.payload["calibration"]
                .as_f64()
                .is_some_and(|calibration| calibration > 1.0)
    }));
    let status = engine.session_context_status(&session.id).await.unwrap();
    let measurement = status.last_measurement.expect("last measurement");
    assert_eq!(measurement.actual_input_tokens, 20_000);
    assert!(measurement.calibration >= 1.0);
    assert!(measurement.samples >= 1);
}

#[test]
fn context_estimator_is_closer_than_the_conservative_budget_estimate() {
    let english = "the quick brown fox jumps over the lazy dog";
    assert!(estimate_context_tokens(english) < estimate_text_tokens(english));
    assert!(estimate_context_tokens(english) >= 8);
    assert_eq!(estimate_context_tokens("你好世界"), 6);
    assert!(estimate_context_tokens("") >= 1);
}

#[test]
fn context_cuts_never_start_at_a_tool_message() {
    let messages = vec![
        ChatMessage::user("one"),
        ChatMessage {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }]),
            tool_call_id: None,
        },
        ChatMessage::tool("call_1", "file contents"),
        ChatMessage {
            role: "assistant".into(),
            content: "done".into(),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage::user("two"),
        ChatMessage {
            role: "assistant".into(),
            content: "again".into(),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    for keep in 2..messages.len() {
        if let Some(cut) = safe_context_cut(&messages, keep, 0) {
            assert_ne!(messages[cut].role, "tool");
            assert!(matches!(messages[cut].role.as_str(), "user" | "assistant"));
        }
    }
}

fn commit_worktree_file(worktree: &Path, name: &str, content: &str) {
    std::fs::write(worktree.join(name), content).unwrap();
    for args in [vec!["add", name], vec!["commit", "-q", "-m", "test commit"]] {
        let status = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
#[tokio::test]
async fn fork_merge_merges_committed_changes_into_source() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let source = engine
        .create_session(project.id, "merge source".into(), None)
        .await
        .unwrap();
    let fork = engine
        .fork_session(&source.id, None, Some("merge fork".into()))
        .await
        .unwrap();
    let source_worktree = PathBuf::from(source.worktree.clone().unwrap());
    let pre_merge = crate::worktree::head_commit(&source_worktree).unwrap();
    let fork_worktree = PathBuf::from(fork.session.worktree.clone().unwrap());
    commit_worktree_file(&fork_worktree, "merged.txt", "from fork\n");

    let result = service
        .merge_fork_now(&source.id, &fork.session.id)
        .await
        .unwrap();
    assert_eq!(result.status, "merged");
    assert_eq!(result.pre_merge_commit, pre_merge);
    assert!(result.merge_commit.is_some());
    assert!(
        std::fs::read_to_string(source_worktree.join("merged.txt"))
            .unwrap()
            .contains("from fork")
    );
    assert_ne!(
        crate::worktree::head_commit(&source_worktree).unwrap(),
        pre_merge
    );
    let events = engine.list_events(source.id.clone(), 0, 300).await.unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.fork.merge"
            && event.payload["status"] == "merged"
            && event.payload["fork_session_id"] == fork.session.id
    }));
}

#[tokio::test]
async fn fork_merge_conflict_aborts_and_restores_source() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let source = engine
        .create_session(project.id, "conflict source".into(), None)
        .await
        .unwrap();
    let source_worktree = PathBuf::from(source.worktree.clone().unwrap());
    let fork = engine
        .fork_session(&source.id, None, Some("conflict fork".into()))
        .await
        .unwrap();
    let fork_worktree = PathBuf::from(fork.session.worktree.clone().unwrap());
    commit_worktree_file(&source_worktree, "README.md", "source side\n");
    commit_worktree_file(&fork_worktree, "README.md", "fork side\n");
    let pre_merge = crate::worktree::head_commit(&source_worktree).unwrap();

    let result = service
        .merge_fork_now(&source.id, &fork.session.id)
        .await
        .unwrap();
    assert_eq!(result.status, "conflict");
    assert!(result.conflicts.iter().any(|path| path == "README.md"));
    assert_eq!(
        crate::worktree::head_commit(&source_worktree).unwrap(),
        pre_merge
    );
    assert_eq!(
        std::fs::read_to_string(source_worktree.join("README.md"))
            .unwrap()
            .trim(),
        "source side"
    );
    let events = engine.list_events(source.id.clone(), 0, 300).await.unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.fork.merge" && event.payload["status"] == "conflict"
    }));
}

#[tokio::test]
async fn fork_merge_rejects_a_dirty_source_worktree() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let source = engine
        .create_session(project.id, "dirty source".into(), None)
        .await
        .unwrap();
    let fork = engine
        .fork_session(&source.id, None, Some("dirty fork".into()))
        .await
        .unwrap();
    let source_worktree = PathBuf::from(source.worktree.clone().unwrap());
    std::fs::write(source_worktree.join("dirty.txt"), "not committed\n").unwrap();
    let error = service
        .merge_fork_now(&source.id, &fork.session.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted changes"));
}

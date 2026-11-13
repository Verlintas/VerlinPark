use super::*;
use crate::tools::AgentTool;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use vp_providers::{EventStream, ProviderError};
use vp_store::Store;
use vp_store::store::Claim;

/// Four-tier delegation polls several nested boxed run loops. Debug builds
/// need more than the harness default 2 MiB stack, so this one test owns a
/// bounded 8 MiB thread instead of changing global test settings.
fn run_with_large_stack<F, Fut>(build: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(build());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn prices(input: f64, output: f64) -> RolePrices {
    RolePrices {
        input,
        output,
        cache_read: input,
        cache_write: input,
    }
}

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

struct RecordingProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
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
        approval_timeout_secs: 600,
        compact_after_tokens: 0,
        compact_keep_messages: 8,
        compact_keep_tokens: 1_000_000,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    };
    let service = AgentService::configured(engine.clone(), config);
    (engine, service, project)
}

fn help_spec(role: AgentRole, profile: AgentProfile) -> AgentSpec {
    AgentSpec {
        id: format!("help-{}", role.as_str()),
        role,
        name: format!("Help {}", role.default_name()),
        depth: 1,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile,
        allow_delegation: false,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("/tmp/help-worktree"),
        branch: "help".to_string(),
        session_worktree: PathBuf::from("/tmp/help-worktree"),
        session_branch: "help".to_string(),
        write_scope: Vec::new(),
        base_commit: "0".repeat(40),
        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    }
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
    config.context_window = 32_000;
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

struct ScoutGuardProvider {
    calls: std::sync::atomic::AtomicUsize,
    paused: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[async_trait]
impl Provider for ScoutGuardProvider {
    fn id(&self) -> &str {
        "scout-guard"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let events = match call {
            0 => vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_scout".into(),
                    name: "scout".into(),
                    arguments: serde_json::json!({
                        "objective": "map the readme",
                        "files": ["README.md"],
                        "max_steps": 3
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            1 => vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            2 => {
                self.paused.notify_one();
                self.resume.notified().await;
                vec![
                    StreamEvent::TextDelta("README.md:1 has the test greeting.".into()),
                    StreamEvent::Done,
                ]
            }
            _ => vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct ScoutCancelProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Provider for ScoutCancelProvider {
    fn id(&self) -> &str {
        "scout-cancel"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            return Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "call_scout".into(),
                    name: "scout".into(),
                    arguments: serde_json::json!({
                        "objective": "map the readme",
                        "max_steps": 2
                    })
                    .to_string(),
                })),
                Ok(StreamEvent::Done),
            ])));
        }
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

/// Emits partial text and provider-reported usage, then stalls. A preemption
/// must persist the reported usage (not an estimate) and never replay the turn.
struct PreemptUsageProvider;

#[async_trait]
impl Provider for PreemptUsageProvider {
    fn id(&self) -> &str {
        "preempt-usage"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(
            futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("partial answer".into())),
                Ok(StreamEvent::Usage {
                    input_tokens: 40,
                    output_tokens: 5,
                    cache_read_tokens: 8,
                    cache_write_tokens: 2,
                }),
            ])
            .chain(futures_util::stream::pending::<
                Result<StreamEvent, ProviderError>,
            >()),
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

/// Deterministic provider for the plan bundle test. It branches on the
/// advertised tool set instead of guessing from prompt text, which also proves
/// the schema partition is stable for the whole run.
struct BundlePlanProvider {
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    leader_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Provider for BundlePlanProvider {
    fn id(&self) -> &str {
        "bundle-plan"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let last_role = request.messages.last().map(|message| message.role.as_str());
        let last_content = request
            .messages
            .last()
            .map(|message| message.content.as_str())
            .unwrap_or_default();
        let events = if names.contains("submit_verdict") {
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "bundle_verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "the trivial task wrote the expected file"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ]
        } else if names.contains("submit_team_plan") {
            let call = self
                .leader_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                let plan = serde_json::json!({
                    "goal": "run a trivial bundle task",
                    "departments": [{
                        "name": "impl",
                        "manager_title": "Implementation",
                        "profile": "coding",
                        "objective": "make the tiny change"
                    }],
                    "tasks": [{
                        "id": "T1",
                        "department": "impl",
                        "title": "Tiny change",
                        "kind": "trivial",
                        "deps": [],
                        "acceptance": ["bundle-note.txt contains hello"],
                        "write_scope": ["bundle-note.txt"]
                    }]
                });
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "bundle_plan".into(),
                        name: "submit_team_plan".into(),
                        arguments: plan.to_string(),
                    }),
                    StreamEvent::Done,
                ]
            } else if call == 1 && last_role == Some("tool") {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "bundle_run".into(),
                        name: "run_team_plan".into(),
                        arguments: "{}".into(),
                    }),
                    StreamEvent::Done,
                ]
            } else {
                vec![
                    StreamEvent::TextDelta("bundle plan complete".into()),
                    StreamEvent::Done,
                ]
            }
        } else if names.contains("write_file") {
            if last_role == Some("tool") {
                vec![
                    StreamEvent::TextDelta("trivial task complete".into()),
                    StreamEvent::Done,
                ]
            } else {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "bundle_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "bundle-note.txt",
                            "content": "hello from the minimal bundle\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ]
            }
        } else {
            vec![
                StreamEvent::TextDelta(format!("unexpected bundle: {last_content}")),
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
    let _ = wait_until_idle(&engine, &session).await;
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
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let todos = engine.list_todos(&session.id).await.unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[1].status, "in_progress");
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "todo.updated"));
}

#[test]
fn delegates_through_four_tiers() {
    run_with_large_stack(|| async {
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
            events.iter().any(
                |event| event.kind == "agent.message" && event.payload["text"] == "leader done"
            )
        );
    });
}

#[tokio::test]
async fn contract_denies_a_runtime_tool_outside_the_worker_surface() {
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
                        "objective": "try a verifier-only tool",
                        "write_scope": ["worker.txt"]
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
                        "summary": "a Worker cannot submit a verifier verdict"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker stopped".into()),
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
        .create_session(project.id, "contract denied tool".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "contract denied tool")
        .await
        .unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let denied = events
        .iter()
        .find(|event| {
            event.kind == "tool.finished"
                && event.payload["agent_role"] == "worker"
                && event.payload["name"] == "submit_verdict"
        })
        .expect("the Worker's verdict attempt is recorded");
    assert_eq!(denied.payload["is_error"], true);
    let output = denied.payload["output"].as_str().unwrap_or_default();
    assert!(output.contains("CONTRACT_TOOL_DENIED"), "{output}");
    assert!(
        events.iter().any(|event| {
            event.kind == "intent.decision"
                && event.payload["role"] == "worker"
                && event.payload["tool"] == "submit_verdict"
                && event.payload["decision"] == "deny"
                && event.payload["rule"] == "tool.membership"
        }),
        "the contract decision must be event-backed"
    );
    assert!(
        !events.iter().any(|event| {
            event.kind == "approval.requested" && event.payload["tool"] == "submit_verdict"
        }),
        "a contract denial must not ask for approval"
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
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::ToolCall(ToolCall {
                id: "retry_verdict".into(),
                name: "submit_verdict".into(),
                arguments: serde_json::json!({
                    "approved": true,
                    "summary": "rebased worker change matches the task"
                })
                .to_string(),
            }),
            StreamEvent::Done,
        ]])),
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
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "idle");
    let integration_memories = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("integration"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(integration_memories.len(), 1);
    assert_eq!(
        integration_memories[0].title,
        format!("Integration {} resolution", queue[0].id)
    );
    assert_eq!(integration_memories[0].source, "orchestrator");
    assert!(integration_memories[0].body.starts_with("resolved: true"));
    assert!(integration_memories[0].body.contains("outcome: merged;"));
    assert!(
        integration_memories[0]
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "commit" && evidence.reference == resolved.commit })
    );
    assert!(
        integration_memories[0]
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "task" && evidence.reference == "T1" })
    );
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
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: branch,
        write_scope: vec!["README.md".into()],
        base_commit: task.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
    // Poll for the terminal worker state instead of sleeping for a fixed
    // interval; the full workspace suite runs many concurrent provider tests.
    for _ in 0..100 {
        let agents = engine.list_agents(&session.id).await.unwrap();
        if agents
            .iter()
            .any(|agent| agent.role == "worker" && agent.status == "changes_requested")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

#[tokio::test]
async fn scout_tool_schema_is_read_only() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let config = service.config.read().unwrap().as_ref().unwrap().clone();
    let names = |role: AgentRole, profile: AgentProfile, allow: bool| {
        tool_schemas(&config, role, profile, allow)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::HashSet<_>>()
    };
    let scout = names(AgentRole::Scout, AgentProfile::Research, false);
    for required in [
        "read_file",
        "read_files",
        "find_files",
        "file_info",
        "grep",
        "list_dir",
        "repo_map",
        "find_symbol",
        "find_references",
        "dependency_graph",
        "affected_tests",
        "code_diagnostics",
        "build_context",
        "git",
        "list_sources",
        "search_sources",
        "read_source",
        "recall",
        "team_status",
    ] {
        assert!(scout.contains(required), "Scout is missing {required}");
    }
    for forbidden in [
        "shell",
        "write_file",
        "edit_file",
        "apply_patch",
        "file_manage",
        "process",
        "wait_for",
        "delegate",
        "delegate_many",
        "delegate_acp",
        "mcp",
        "browser",
        "remember",
        "publish_artifact",
        "publish_artifact_file",
        "send_message",
        "ask_user",
        "coordinate",
        "use_skill",
        "scout",
    ] {
        assert!(
            !scout.contains(forbidden),
            "Scout must not advertise {forbidden}"
        );
        assert!(!AgentRole::Scout.allows_tool(AgentProfile::Research, forbidden));
    }
    for role in [AgentRole::Leader, AgentRole::Manager] {
        let tools = names(role, AgentProfile::Coding, true);
        assert!(tools.contains("scout"), "{role:?} cannot call scout");
    }
}

#[tokio::test]
async fn computer_tool_schema_is_limited_to_leader_and_coding_workers() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let config = service.config.read().unwrap().as_ref().unwrap().clone();
    let names = |role: AgentRole, profile: AgentProfile| {
        tool_schemas(&config, role, profile, true)
            .into_iter()
            .map(|tool| tool.name)
            .collect::<std::collections::HashSet<_>>()
    };
    assert!(names(AgentRole::Leader, AgentProfile::Coding).contains("computer"));
    assert!(names(AgentRole::Worker, AgentProfile::Coding).contains("computer"));
    assert!(names(AgentRole::Labour, AgentProfile::Coding).contains("computer"));
    assert!(!names(AgentRole::Manager, AgentProfile::Coding).contains("computer"));
    assert!(!names(AgentRole::Verifier, AgentProfile::Verification).contains("computer"));
    assert!(!names(AgentRole::Scout, AgentProfile::Research).contains("computer"));
    assert!(!names(AgentRole::Worker, AgentProfile::Research).contains("computer"));
}

#[test]
fn scout_input_is_bounded() {
    assert!(parse_scout(&serde_json::json!({"objective": ""})).is_err());
    assert!(parse_scout(&serde_json::json!({"objective": "x".repeat(4_001)})).is_err());
    let default = parse_scout(&serde_json::json!({"objective": "map it"})).unwrap();
    assert_eq!(default.max_steps, 6);
    assert_eq!(
        parse_scout(&serde_json::json!({"objective": "map it", "max_steps": 99}))
            .unwrap()
            .max_steps,
        12
    );
    assert_eq!(
        parse_scout(&serde_json::json!({"objective": "map it", "max_steps": 0}))
            .unwrap()
            .max_steps,
        1
    );
    let files = (0..21)
        .map(|index| format!("src/{index}.rs"))
        .collect::<Vec<_>>();
    assert!(parse_scout(&serde_json::json!({"objective": "map it", "files": files})).is_err());
}

#[tokio::test]
async fn scout_returns_a_bounded_brief_without_a_worktree_or_merge_entry() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_scout".into(),
                    name: "scout".into(),
                    arguments: serde_json::json!({
                        "objective": "map the readme",
                        "files": ["README.md"],
                        "max_steps": 3
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
                StreamEvent::TextDelta("README.md:1 contains the test greeting.".into()),
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
        .create_session(project.id, "scout the readme".into(), None)
        .await
        .unwrap();
    let session_worktree = session.worktree.clone().unwrap();
    let session_branch = session.branch.clone().unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "scout the readme")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let agents = engine.list_agents(&session.id).await.unwrap();
    let scout = agents
        .iter()
        .find(|agent| agent.role == "scout")
        .expect("a Scout agent record");
    assert_eq!(scout.status, "idle");
    assert_eq!(scout.title.as_deref(), Some("Scout"));
    assert_eq!(scout.profile.as_deref(), Some("research"));
    assert_eq!(scout.worktree.as_deref(), Some(session_worktree.as_str()));
    assert_eq!(scout.branch.as_deref(), Some(session_branch.as_str()));
    assert!(scout.task_id.is_none());
    let leader = agents.iter().find(|agent| agent.role == "leader").unwrap();
    assert_eq!(scout.parent_id.as_deref(), Some(leader.id.as_str()));
    assert!(
        engine
            .list_merge_queue(&session.id)
            .await
            .unwrap()
            .is_empty(),
        "a Scout must never enqueue a merge"
    );

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.spawned"
            && event.payload["agent_role"] == "scout"
            && event.payload["read_only"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.started"
            && event.payload["agent_role"] == "scout"
            && event.payload["read_only"] == true
            && event.payload["max_steps"] == 3
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "scout"
            && event.payload["status"] == "idle"
    }));
    let completed = events
        .iter()
        .find(|event| event.kind == "scout.completed")
        .expect("scout.completed event");
    assert_eq!(completed.payload["agent_role"], "scout");
    assert_eq!(completed.payload["status"], "completed");
    assert_eq!(completed.payload["cancelled"], false);
    assert!(
        completed.payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("README.md:1"))
    );
    assert!(completed.payload["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert!(
        completed.payload["usage"]["output_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(completed.payload["usage"]["calls"].as_u64().unwrap() >= 2);
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "merge_queue.enqueued")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn scout_cannot_execute_write_or_shell_tools() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_scout".into(),
                    name: "scout".into(),
                    arguments: serde_json::json!({
                        "objective": "try to write",
                        "max_steps": 3
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_shell".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({
                        "command": "printf 'tampered\\n' > scout-denied.txt"
                    })
                    .to_string(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "call_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "scout-denied.txt",
                        "content": "tampered\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("I cannot write as a read-only Scout.".into()),
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
        .create_session(project.id, "scout write attempt".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "scout write attempt")
        .await
        .unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    assert!(
        !std::path::Path::new(&worktree)
            .join("scout-denied.txt")
            .exists(),
        "a Scout tool call must not write"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    for tool in ["shell", "write_file"] {
        assert!(events.iter().any(|event| {
            event.kind == "tool.finished"
                && event.payload["agent_role"] == "scout"
                && event.payload["name"] == tool
                && event.payload["is_error"] == true
                && event.payload["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("not available to Scout"))
        }));
    }
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "approval.requested"),
        "Scout policy denials must not ask the user for approval"
    );
    let scout = engine
        .list_agents(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.role == "scout")
        .unwrap();
    assert_eq!(scout.status, "idle");
}

#[tokio::test]
async fn scout_post_run_guard_rejects_worktree_changes() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(ScoutGuardProvider {
        calls: std::sync::atomic::AtomicUsize::new(0),
        paused: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let trait_provider: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, trait_provider);
    let session = engine
        .create_session(project.id, "scout guard".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "scout guard").await.unwrap();
    provider.paused.notified().await;
    std::fs::write(
        std::path::Path::new(&worktree).join("README.md"),
        "tampered\n",
    )
    .unwrap();
    std::fs::write(
        std::path::Path::new(&worktree).join("scout-untracked.txt"),
        "untracked\n",
    )
    .unwrap();
    provider.resume.notify_one();
    let _ = wait_until_idle(&engine, &session).await;

    let scout = engine
        .list_agents(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.role == "scout")
        .unwrap();
    assert_eq!(scout.status, "failed");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let completed = events
        .iter()
        .find(|event| event.kind == "scout.completed")
        .expect("scout.completed event");
    assert_eq!(completed.payload["status"], "rejected");
    assert_eq!(completed.payload["guard_failed"], true);
    let modified = completed.payload["modified_paths"]
        .as_array()
        .expect("modified paths");
    assert!(
        modified
            .iter()
            .any(|path| path.as_str().is_some_and(|path| path.contains("README.md")))
    );
    assert!(modified.iter().any(|path| {
        path.as_str()
            .is_some_and(|path| path.contains("scout-untracked.txt"))
    }));
    assert_eq!(
        std::fs::read_to_string(std::path::Path::new(&worktree).join("README.md")).unwrap(),
        "tampered\n"
    );
    assert!(
        engine
            .list_merge_queue(&session.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn cancelling_the_parent_cancels_a_running_scout() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(ScoutCancelProvider {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let trait_provider: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, trait_provider);
    let session = engine
        .create_session(project.id, "cancel scout".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "cancel scout").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let agents = engine.list_agents(&session.id).await.unwrap();
        if agents
            .iter()
            .any(|agent| agent.role == "scout" && agent.status == "running")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Scout did not start: {agents:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(service.cancel(&session.id));
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "cancelled");
    let scout = engine
        .list_agents(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.role == "scout")
        .unwrap();
    assert_eq!(scout.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "scout.completed"
            && event.payload["cancelled"] == true
            && event.payload["status"] == "cancelled"
    }));
}

#[test]
fn scout_slots_and_tokens_share_the_session_budget() {
    let caps = AgentCaps {
        managers: 1,
        workers: 1,
        labour: 1,
    };
    let budget = SessionBudget::new(Some(100), 0, caps);
    let first = budget.reserve_slot(AgentRole::Scout).unwrap();
    let second = budget.reserve_slot(AgentRole::Scout).unwrap();
    let third = budget.reserve_slot(AgentRole::Scout).unwrap();
    assert!(
        budget.reserve_slot(AgentRole::Scout).is_none(),
        "the Scout cap must be bounded"
    );
    assert_eq!(budget.available(AgentRole::Scout), 0);
    drop(third);
    assert_eq!(budget.available(AgentRole::Scout), 1);
    assert!(
        budget.reserve_tokens(101).is_none(),
        "Scout calls must respect the session token budget"
    );
    let reservation = budget.reserve_tokens(100).unwrap();
    assert_eq!(budget.available_tokens(), Some(0));
    drop(reservation);
    drop(second);
    drop(first);
    assert_eq!(budget.available(AgentRole::Scout), 3);
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
    let _ = wait_until_idle(&engine, &session).await;

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

#[tokio::test]
async fn provider_cache_usage_reaches_ledger_and_session_totals() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("cached answer".into()),
            StreamEvent::Usage {
                input_tokens: 1_000,
                output_tokens: 100,
                cache_read_tokens: 600,
                cache_write_tokens: 100,
            },
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "cache usage".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "cache usage").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 1);
    let entry = &ledger.entries[0];
    assert_eq!(entry.input_tokens, 1_000);
    assert_eq!(entry.output_tokens, 100);
    assert_eq!(entry.cache_read_tokens, 600);
    assert_eq!(entry.cache_write_tokens, 100);
    assert_eq!(ledger.totals.cache_read_tokens, 600);
    assert_eq!(ledger.totals.cache_write_tokens, 100);
    assert!((ledger.totals.cache_hit_ratio - 0.6).abs() < 1e-9);

    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].tokens_in, 1_000);
    assert_eq!(sessions[0].tokens_out, 100);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let usage = events
        .iter()
        .find(|event| event.kind == "agent.usage")
        .expect("usage event");
    assert_eq!(usage.payload["input_tokens"], 1_000);
    assert_eq!(usage.payload["cache_read_tokens"], 600);
    assert_eq!(usage.payload["cache_write_tokens"], 100);

    let cache_event = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("context.cache event");
    assert_eq!(cache_event.payload["cache_read_tokens"], 600);
    assert_eq!(cache_event.payload["cache_write_tokens"], 100);
    assert_eq!(cache_event.payload["cache_hit_ratio"], 0.6);
    assert!(cache_event.payload["prompt_prefix_hash"].is_string());
    assert!(cache_event.payload.get("prompt").is_none());

    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix event");
    assert!(
        prefix.payload["preamble_tokens_estimate"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.input_tokens, 1_000);
    assert_eq!(status.cache_read_tokens, 600);
    assert!((status.cache_hit_ratio - 0.6).abs() < 1e-9);
    assert!(status.prompt_prefix.is_some());
    assert!(status.preamble_tokens_estimate.is_some());
    assert!(status.last_call_at.is_some());
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
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: false,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("/tmp"),
        branch: "task".into(),
        session_worktree: PathBuf::from("/tmp"),
        session_branch: "session".into(),
        write_scope: Vec::new(),
        base_commit: "base".into(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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

#[test]
fn unlimited_flag_bypasses_budget_gates_and_can_be_revoked() {
    let flag = Arc::new(AtomicBool::new(true));
    let budget = SessionBudget::new(
        Some(10),
        10,
        AgentCaps {
            managers: 1,
            workers: 1,
            labour: 1,
        },
    )
    .with_cost(Some(1.0), 2.0)
    .with_time(Some(1))
    .with_unlimited_flag(flag.clone());
    assert!(!budget.exhausted());
    assert!(budget.warning().is_none());
    assert!(budget.reserve_tokens(100).is_none());
    assert!(
        budget
            .reserve_cost(100, 100, prices(1.0, 1.0))
            .expect("unlimited cost reservation")
            .is_none()
    );
    assert!(budget.can_expand_workers());
    flag.store(false, Ordering::SeqCst);
    assert_eq!(budget.exceeded_reason(), Some("tokens"));
    assert!(budget.exhausted());
}

#[tokio::test]
async fn unlimited_mode_keeps_a_zero_headroom_session_running() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("large project completed".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session_with_limits(
            project.id,
            "large engineering project".into(),
            Some(1),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(session.token_budget, Some(1));
    let toggled = engine
        .set_session_unlimited(&session.id, true)
        .await
        .unwrap();
    assert!(toggled.unlimited);
    service
        .start(&session.id, "large engineering project")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "agent.budget_exceeded")
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "large project completed"
    }));
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

    let mut read_only_worker = base.clone();
    read_only_worker["departments"][0]["profile"] = serde_json::json!("generic");
    read_only_worker["tasks"][0]["write_scope"] = serde_json::json!(["src/lib.rs"]);
    assert!(
        validate_team_plan(&serde_json::from_value(read_only_worker).unwrap(), 5).is_err(),
        "a write task must not be assigned to a generic read-only Worker"
    );

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
    assert_eq!(usage_cost(1_000_000, 500_000, 0, 0, prices(2.0, 4.0)), 4.0);
}

#[test]
fn cache_prices_bill_disjoint_input_subsets_and_fall_back_to_input_price() {
    let cached = RolePrices {
        input: 2.0,
        output: 4.0,
        cache_read: 0.2,
        cache_write: 2.5,
    };
    // 500k uncached input + 400k cache reads + 100k cache writes + 500k output.
    let cost = usage_cost(1_000_000, 500_000, 400_000, 100_000, cached);
    assert!((cost - (1.0 + 0.08 + 0.25 + 2.0)).abs() < 1e-9);

    let fallback = RolePrices {
        input: 2.0,
        output: 4.0,
        cache_read: 2.0,
        cache_write: 2.0,
    };
    assert_eq!(
        usage_cost(1_000_000, 500_000, 400_000, 100_000, fallback),
        usage_cost(1_000_000, 500_000, 0, 0, fallback)
    );

    // Inconsistent provider reports are clamped so cache subsets never
    // exceed the total input or get billed twice.
    assert_eq!(
        usage_cost(1_000_000, 0, 900_000, 900_000, cached),
        usage_cost(1_000_000, 0, 900_000, 100_000, cached)
    );
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

#[test]
fn cost_reservations_bound_concurrent_calls() {
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 5,
            workers: 10,
            labour: 20,
        },
    )
    .with_cost(Some(1.0), 0.0);
    let first = budget
        .reserve_cost(400_000, 400_000, prices(1.0, 1.0))
        .expect("first cost reservation")
        .expect("cost limit configured");
    assert!(
        budget
            .reserve_cost(200_000, 200_000, prices(1.0, 1.0))
            .is_err()
    );
    assert!(!first.settle(0.6));
    assert!((budget.cost_spent() - 0.6).abs() < 1e-9);
    let second = budget
        .reserve_cost(100_000, 100_000, prices(1.0, 1.0))
        .expect("remaining cost reservation")
        .expect("cost limit configured");
    assert!(second.settle(0.4));
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

struct SteeringProvider {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
    requests: Mutex<Vec<ChatRequest>>,
}

#[async_trait]
impl Provider for SteeringProvider {
    fn id(&self) -> &str {
        "steering"
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "steer-read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                })),
                Ok(StreamEvent::Done),
            ])))
        } else {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("steering honored".into())),
                Ok(StreamEvent::Done),
            ])))
        }
    }
}

struct GatedFinalProvider {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl Provider for GatedFinalProvider {
    fn id(&self) -> &str {
        "gated-final"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::Done),
        ])))
    }
}

#[tokio::test]
async fn steering_is_delivered_at_the_next_turn_boundary() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(SteeringProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
        requests: Mutex::new(Vec::new()),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "steering".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "steering").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    let steering = engine
        .steer_session(&session.id, "inspect the tests first", None, None, None)
        .await
        .unwrap()
        .steering;
    provider.release.notify_one();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    {
        let requests = provider.requests.lock().unwrap();
        assert!(requests.len() >= 2, "expected a second provider call");
        assert!(requests[1].messages.iter().any(|message| {
            message.role == "user"
                && message
                    .content
                    .starts_with(&format!("[Operator steering #{}]", steering.id))
                && message.content.contains("inspect the tests first")
        }));
    }
    let listed = engine
        .session_steering(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(listed.steering[0].status, "delivered");
    assert!(listed.steering[0].delivered_at.is_some());
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "steering.delivered")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.steering_matches, "{replay:?}");
}

#[tokio::test]
async fn steering_left_pending_when_a_run_ends_is_cancelled() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(GatedFinalProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "cleanup".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "cleanup").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    let steering = engine
        .steer_session(&session.id, "too late", None, None, None)
        .await
        .unwrap()
        .steering;
    provider.release.notify_one();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let listed = engine
        .session_steering(&session.id, None, Some(10))
        .await
        .unwrap();
    let steering = listed
        .steering
        .iter()
        .find(|message| message.id == steering.id)
        .unwrap();
    assert_eq!(steering.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "steering.cancelled")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "steering.delivered")
    );
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
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from(session.worktree.clone().unwrap()),
        branch: session.branch.clone().unwrap(),
        session_worktree: PathBuf::from(session.worktree.clone().unwrap()),
        session_branch: session.branch.clone().unwrap(),
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
async fn coordination_tool_records_blockers_and_team_status_reads_the_board() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "coordinate".into(), None)
        .await
        .unwrap();
    let leader = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap_or_default());
    let branch = session.branch.clone().unwrap_or_default();
    let spec = AgentSpec {
        id: leader.clone(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: branch.clone(),
        session_worktree: worktree,
        session_branch: branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let plan: TeamPlan = serde_json::from_value(serde_json::json!({
        "goal": "coordinate",
        "departments": [{
            "name": "Platform",
            "manager_title": "Platform",
            "profile": "coding",
            "objective": "build"
        }],
        "tasks": [{
            "id": "T1",
            "department": "Platform",
            "title": "Build the bridge",
            "kind": "coding",
            "deps": [],
            "acceptance": ["done"]
        }]
    }))
    .unwrap();
    engine.set_team_plan(&session.id, &plan).await.unwrap();

    let output = run_coordination_tool(
        &engine,
        &session,
        &spec,
        serde_json::json!({
            "action": "blocker",
            "task_id": "T1",
            "message": "waiting for the API schema",
            "severity": "warning"
        }),
    )
    .await
    .unwrap();
    assert!(!output.is_error, "{}", output.content);

    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "coordination.note"
            && event.payload["action"] == "blocker"
            && event.payload["task_id"] == "T1"
            && event.payload["agent_id"] == leader
    }));

    let listed = run_coordination_tool(
        &engine,
        &session,
        &spec,
        serde_json::json!({"action": "list"}),
    )
    .await
    .unwrap();
    assert!(listed.content.contains("waiting for the API schema"));

    let status = run_team_status(&engine, &session, serde_json::json!({}))
        .await
        .unwrap();
    assert!(!status.is_error, "{}", status.content);
    assert!(status.content.contains("Tasks: 0/1 complete"));
    assert!(status.content.contains("Ready: T1"));
    assert!(status.content.contains("Recent coordination"));
    assert!(status.content.contains("waiting for the API schema"));
}

#[test]
fn shell_secret_roots_cover_database_sidecars_and_artifacts() {
    let data = TempDir::new().unwrap();
    for relative in [
        "verlinpark.db",
        "verlinpark.db-wal",
        "verlinpark.db-shm",
        "verlinpark.db-journal",
        "verlinpark.db.pre-rebuild-20261109",
        "projects.jsonl",
        "artifacts/blobs/aa/bb",
    ] {
        let path = data.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "secret").unwrap();
    }
    let roots = shell_secret_roots(data.path());
    for relative in [
        "verlinpark.db",
        "verlinpark.db-wal",
        "verlinpark.db-shm",
        "verlinpark.db-journal",
        "verlinpark.db.pre-rebuild-20261109",
        "projects.jsonl",
        "artifacts",
    ] {
        assert!(
            roots.iter().any(|root| root == &data.path().join(relative)),
            "missing secret root {relative}"
        );
    }
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
    assert_eq!(model_for(&config, AgentRole::Scout), "test");
    config.role_models.scout.model = Some("scout-model".into());
    assert_eq!(model_for(&config, AgentRole::Scout), "scout-model");
    let worker_prices = role_prices(&config, AgentRole::Worker);
    assert_eq!(worker_prices, prices(2.0, 4.0));
    config.role_models.worker.cache_read_usd_per_million = Some(0.2);
    config.role_models.worker.cache_write_usd_per_million = Some(2.5);
    let cached = role_prices(&config, AgentRole::Worker);
    assert_eq!(cached.cache_read, 0.2);
    assert_eq!(cached.cache_write, 2.5);
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

            if system.contains("You are a Verifier") {
                return tool_sse(
                    "verdict",
                    "submit_verdict",
                    serde_json::json!({
                        "approved": true,
                        "summary": "HTTP worker output is correct"
                    }),
                );
            }
            if system.contains("You are a Worker") {
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
            let system = body["system"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    body["system"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let last = messages.last();
            let last_content = last
                .map(|message| content_text(&message["content"]))
                .unwrap_or_default();
            let tool_result = last
                .map(|message| message["content"].is_array())
                .unwrap_or(false);

            if system.contains("You are a Verifier") {
                return tool_sse(
                    "verdict",
                    "submit_verdict",
                    serde_json::json!({
                        "approved": true,
                        "summary": "Anthropic worker output is correct"
                    }),
                );
            }
            if system.contains("You are a Worker") {
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
    assert!(events.iter().any(|event| {
        event.kind == "approval.requested"
            && event.payload["tool"] == "mcp"
            && event.payload["approval_risk"] == "host_state"
    }));
}

#[tokio::test]
async fn managers_and_verifiers_cannot_execute_mcp_tools() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, _project) = test_service(&dir);
    for role in [AgentRole::Manager, AgentRole::Verifier] {
        let output = run_mcp_tool(
            &engine,
            role,
            "project-1",
            serde_json::json!({
                "action": "call",
                "server": "missing",
                "tool": "mutate",
                "arguments": {}
            }),
        )
        .await
        .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("cannot execute MCP tools"));
    }
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
            && event.payload["approval_risk"] == "host_state"
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
    // Wait until the run is active before cancelling. Under a loaded test
    // harness a fixed sleep can race the spawned run.
    for _ in 0..200 {
        let active = engine
            .list_events(session.id.clone(), 0, 20)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == "agent.turn_started");
        if active {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(service.cancel(&session.id));
    for _ in 0..200 {
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        if sessions[0].status == "cancelled" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
#[test]
fn delegates_to_an_external_acp_agent_and_integrates_it() {
    run_with_large_stack(|| async {
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
            events.iter().any(|event| {
                event.kind == "agent.spawned" && event.payload["backend"] == "acp"
            })
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
    });
}

#[tokio::test]
async fn delegates_to_a_remote_acp_agent_with_host_state_approval() {
    use crate::acp::transport_integration::{TestServerConfig, TestServerHandle};
    use vp_protocol::types::{AcpAgentConfig, AcpTransport};

    let server = TestServerHandle::start(TestServerConfig::default()).await;
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_blocked".into(),
                    name: "delegate_acp".into(),
                    arguments: serde_json::json!({
                        "agent": "remote-blocked",
                        "objective": "this must be refused"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_remote".into(),
                    name: "delegate_acp".into(),
                    arguments: serde_json::json!({
                        "agent": "remote-http",
                        "objective": "run on the remote service",
                        "write_scope": ["remote.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    // Bypass add-time validation deliberately: a row created by an older build
    // or a hand edit must still be refused by delegation.
    engine
        .store()
        .add_acp_agent_config(&AcpAgentConfig {
            name: "remote-blocked".into(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            enabled: true,
            auto_approve: false,
            timeout_secs: 120,
            transport: AcpTransport::StreamableHttp,
            endpoint: Some(server.endpoint.clone()),
            auth_header_env: None,
            headers: Default::default(),
            connect_timeout_secs: 5,
            allow_remote: false,
        })
        .unwrap();
    engine
        .add_acp_agent_config(AcpAgentConfig {
            name: "remote-http".into(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            enabled: true,
            auto_approve: false,
            timeout_secs: 120,
            transport: AcpTransport::StreamableHttp,
            endpoint: Some(server.endpoint.clone()),
            auth_header_env: None,
            headers: Default::default(),
            connect_timeout_secs: 5,
            allow_remote: true,
        })
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "remote acp".into(), None)
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
    service.start(&session.id, "remote acp").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    approver.abort();
    assert_eq!(finished.status, "idle");
    assert_eq!(
        server.server.counters.prompts.load(Ordering::SeqCst),
        1,
        "only the explicitly allowed remote agent may prompt"
    );

    let agents = engine.list_agents(&session.id).await.unwrap();
    assert!(
        agents.iter().any(|agent| agent.model == "acp:remote-http"),
        "the allowed remote agent is recorded as a worker: {agents:?}"
    );
    assert!(
        !agents
            .iter()
            .any(|agent| agent.model == "acp:remote-blocked"),
        "a remote agent without allow_remote must never spawn"
    );

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "approval.requested"
            && event.payload["tool"] == "delegate_acp"
            && event.payload["approval_risk"] == "host_state"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.spawned"
            && event.payload["acp_transport"] == "streamable_http"
            && event.payload["acp_sandboxed"] == false
            && event.payload["acp_endpoint"] == server.endpoint
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.message"
            && event.payload["acp_transport"] == "streamable_http"
            && event.payload["acp_resumed"] == false
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "worker"
            && event.payload["merged"] == false
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

#[test]
fn estimate_message_tokens_includes_tool_call_payloads() {
    let dry = ChatMessage::user("hello");
    let with_calls = ChatMessage::assistant_tool_calls(
        "hello",
        vec![vp_providers::ToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: "{\"command\":\"echo hi\"}".into(),
        }],
    );
    assert!(estimate_message_tokens(&with_calls) > estimate_message_tokens(&dry));
}

#[test]
fn keep_tokens_trims_to_a_deterministic_message_boundary() {
    let mut conversation = Vec::new();
    for index in 0..12 {
        conversation.push(if index % 2 == 0 {
            ChatMessage::user("x".repeat(400))
        } else {
            ChatMessage {
                role: "assistant".into(),
                content: "y".repeat(400),
                tool_calls: None,
                tool_call_id: None,
            }
        });
    }
    let count_only = safe_context_cut_with_tokens(&conversation, 8, 0, 0).unwrap();
    let token_limited = safe_context_cut_with_tokens(&conversation, 8, 250, 0).unwrap();
    assert!(token_limited >= count_only);
    assert!(matches!(
        conversation[token_limited].role.as_str(),
        "user" | "assistant"
    ));
    // The retained tail satisfies the token budget unless a single message is
    // already larger than the budget.
    let retained = estimate_messages_tokens(&conversation[token_limited..]);
    assert!(retained <= 500, "retained tail was {retained} tokens");
}

#[test]
fn structured_state_is_deterministic_and_refreshable() {
    let state = ContextStructuredState {
        goal: "ship C1".into(),
        plan_revision: 3,
        active_task_id: Some("T2".into()),
        active_task_title: Some("paging".into()),
        active_task_department: Some("runtime".into()),
        active_task_acceptance: vec!["tests pass".into(), "no path escape".into()],
        active_task_write_scope: vec!["crates/vp-core".into()],
        recent_completed_task_ids: vec!["T1".into()],
        generated_at: "2026-11-11T00:00:00Z".into(),
    };
    let first = render_structured_state(&state);
    let second = render_structured_state(&state);
    assert_eq!(first, second);
    assert!(first.contains("Goal: ship C1"));
    assert!(first.contains("Plan revision: 3"));
    assert!(first.contains("Active task: T2 - paging"));
    assert!(first.contains("tests pass"));
    assert!(first.contains("crates/vp-core"));
    let refreshed = refresh_structured_state(
        &format!("Earlier conversation summary\n{first}\nmodel summary text"),
        &render_structured_state(&ContextStructuredState {
            plan_revision: 4,
            ..state
        }),
    );
    assert_eq!(refreshed.matches(STRUCTURED_STATE_START).count(), 1);
    assert!(refreshed.contains("Plan revision: 4"));
    assert!(refreshed.contains("model summary text"));
}

#[tokio::test]
async fn prepared_summary_cache_invalidates_on_every_key_component() {
    let dir = TempDir::new().unwrap();
    let (engine, service, _project) = test_service(&dir);
    let config = service.config.read().unwrap().as_ref().unwrap().clone();
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let conversation = vec![
        ChatMessage::user("one"),
        ChatMessage {
            role: "assistant".into(),
            content: "two".into(),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage::user("three"),
    ];
    let key = prepared_summary_key(&config, &spec, "prefix-a", 2, &conversation, 3);
    let prepared = PreparedSummary {
        key: key.clone(),
        summary: "summary".into(),
        input_tokens: 10,
        output_tokens: 5,
        estimated_usage: false,
    };
    let mut runtime = CompactionRuntime {
        prepared: Some(prepared.clone()),
        failed_prepare_keys: HashSet::new(),
    };
    let (reused, was_present) = runtime.take_reusable(&config, &spec, "prefix-a", 2, &conversation);
    assert!(reused.is_some());
    assert!(was_present);

    let mut wrong_prefix = CompactionRuntime {
        prepared: Some(prepared.clone()),
        failed_prepare_keys: HashSet::new(),
    };
    assert!(
        wrong_prefix
            .take_reusable(&config, &spec, "prefix-b", 2, &conversation)
            .0
            .is_none()
    );
    let mut wrong_plan = CompactionRuntime {
        prepared: Some(prepared.clone()),
        failed_prepare_keys: HashSet::new(),
    };
    assert!(
        wrong_plan
            .take_reusable(&config, &spec, "prefix-a", 3, &conversation)
            .0
            .is_none()
    );
    let mut mutated = conversation.clone();
    mutated[0].content = "one changed".into();
    let mut wrong_cursor = CompactionRuntime {
        prepared: Some(prepared),
        failed_prepare_keys: HashSet::new(),
    };
    assert!(
        wrong_cursor
            .take_reusable(&config, &spec, "prefix-a", 2, &mutated)
            .0
            .is_none()
    );
    let _ = engine;
}

#[tokio::test]
async fn structured_state_honors_an_explicit_task_id() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "explicit task".into(), None)
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "two tasks".into(),
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
                title: "First".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: Vec::new(),
                write_scope: vec!["src/one".into()],
                acceptance: vec!["one done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
            PlanTask {
                id: "T2".into(),
                department: "impl".into(),
                title: "Second".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: vec!["T1".into()],
                write_scope: vec!["src/two".into()],
                acceptance: vec!["two done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
        ],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let spec = AgentSpec {
        id: "worker".into(),
        role: AgentRole::Worker,
        name: "Worker".into(),
        depth: 1,
        parent_name: Some("Leader".into()),
        title: Some("Implementation".into()),
        task_id: Some("T2".into()),
        department: Some("impl".into()),
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: false,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: vec!["src/two".into()],
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let state = load_compaction_structured_state(&engine, &session, &spec).await;
    assert_eq!(state.goal, "two tasks");
    assert_eq!(state.plan_revision, 1);
    assert_eq!(state.active_task_id.as_deref(), Some("T2"));
    assert_eq!(state.active_task_title.as_deref(), Some("Second"));
    assert_eq!(state.active_task_department.as_deref(), Some("impl"));
    assert_eq!(state.active_task_acceptance, vec!["two done".to_string()]);
    assert_eq!(state.active_task_write_scope, vec!["src/two".to_string()]);
    let rendered = render_structured_state(&state);
    assert!(rendered.contains("Active task: T2 - Second"));
    assert!(rendered.contains("src/two"));
}

#[tokio::test]
async fn structured_state_without_a_plan_uses_the_session_goal() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "session-only goal".into(), None)
        .await
        .unwrap();
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let state = load_compaction_structured_state(&engine, &session, &spec).await;
    assert_eq!(state.goal, "session-only goal");
    assert_eq!(state.plan_revision, 0);
    assert!(state.active_task_id.is_none());
    assert!(state.active_task_acceptance.is_empty());
}

#[tokio::test]
async fn agent_shaping_uses_the_project_tool_output_policy() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "policy shaping".into(), None)
        .await
        .unwrap();
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let mut policy = vp_protocol::types::ToolOutputPolicy {
        spill_threshold_bytes: 100_000,
        preview_bytes: 4096,
        max_total_bytes_per_turn: 500_000,
        ..vp_protocol::types::ToolOutputPolicy::default()
    };
    engine
        .set_tool_output_policy(&project.id, policy.clone())
        .await
        .unwrap();
    let pending = || crate::tool_output::PendingToolOutput {
        tool_call_id: "call_policy".into(),
        tool_name: "shell".into(),
        output: ToolOutput::ok("p".repeat(10_000)),
    };
    let (shaped, _) =
        shape_tool_output_batch(&engine, &session, &spec, &project, 0, vec![pending()])
            .await
            .unwrap();
    assert!(shaped[0].output.output_ref.is_none());
    policy.spill_threshold_bytes = 512;
    policy.preview_bytes = 256;
    policy.max_total_bytes_per_turn = 500_000;
    engine
        .set_tool_output_policy(&project.id, policy)
        .await
        .unwrap();
    let (shaped, _) =
        shape_tool_output_batch(&engine, &session, &spec, &project, 0, vec![pending()])
            .await
            .unwrap();
    assert!(shaped[0].output.output_ref.is_some());
}

#[tokio::test]
async fn error_failure_contract_survives_output_spill_shaping() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "error shaping".into(), None)
        .await
        .unwrap();
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let policy = vp_protocol::types::ToolOutputPolicy {
        spill_threshold_bytes: 256,
        preview_bytes: 128,
        max_total_bytes_per_turn: 500_000,
        ..vp_protocol::types::ToolOutputPolicy::default()
    };
    engine
        .set_tool_output_policy(&project.id, policy)
        .await
        .unwrap();
    let mut output = ToolOutput::error_coded(
        "shell",
        crate::tooling::errors::ToolErrorCode::COMMAND_FAILED,
        "command failed with a long diagnostic body",
    )
    .with_failure_evidence("exit_code=2");
    output
        .content
        .push_str(&format!("\n{}", "diagnostic ".repeat(2_000)));
    output.total_bytes = output.content.len() as u64;
    output.preview_bytes = output.total_bytes;
    let pending = crate::tool_output::PendingToolOutput {
        tool_call_id: "call_error_spill".into(),
        tool_name: "shell".into(),
        output,
    };
    let (mut shaped, _) =
        shape_tool_output_batch(&engine, &session, &spec, &project, 0, vec![pending])
            .await
            .unwrap();
    let output = &mut shaped[0].output;
    assert!(output.is_error);
    assert!(output.output_ref.is_some());
    assert!(output.truncated);
    assert!(output.content.len() < output.total_bytes as usize);
    assert!(output.content.starts_with("error[COMMAND_FAILED]"));
    let failure = output.failure.as_ref().expect("failure contract");
    assert_eq!(
        failure.code,
        crate::tooling::errors::ToolErrorCode::COMMAND_FAILED
    );
    assert!(!failure.retryable);
    let structured = output.structured.as_ref().unwrap();
    assert_eq!(structured["code"], "COMMAND_FAILED");
    assert_eq!(structured["retryable"], false);
    assert_eq!(structured["evidence"], "exit_code=2");
    output.annotate_shape_metadata();
    let annotated = output.structured.as_ref().unwrap();
    assert_eq!(annotated["code"], "COMMAND_FAILED");
    assert_eq!(annotated["truncated"], true);
    assert!(annotated["output_ref"].as_str().is_some());
}

#[tokio::test]
async fn read_tool_output_is_advertised_to_every_read_only_role() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let config = service.config.read().unwrap().as_ref().unwrap().clone();
    assert!(is_parallel_safe_tool("read_tool_output"));
    for (role, profile) in [
        (AgentRole::Leader, AgentProfile::Generic),
        (AgentRole::Manager, AgentProfile::Coding),
        (AgentRole::Verifier, AgentProfile::Verification),
        (AgentRole::Scout, AgentProfile::Research),
        (AgentRole::Worker, AgentProfile::Coding),
        (AgentRole::Labour, AgentProfile::Generic),
    ] {
        assert!(
            role.allows_tool(profile, "read_tool_output"),
            "{role:?} must allow read_tool_output"
        );
        let schemas = tool_schemas(&config, role, profile, role == AgentRole::Leader);
        assert!(
            schemas
                .iter()
                .any(|schema| schema.name == "read_tool_output"),
            "{role:?} schema must include read_tool_output"
        );
    }
}

#[tokio::test]
async fn spills_large_tool_results_and_pages_them_with_read_tool_output() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "tool output".into(), None)
        .await
        .unwrap();
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let content = (0..1_000)
        .map(|index| format!("line {index:04} value {}", "x".repeat(20)))
        .collect::<Vec<_>>()
        .join("\n");
    let pending = vec![crate::tool_output::PendingToolOutput {
        tool_call_id: "call_shell".into(),
        tool_name: "shell".into(),
        output: ToolOutput::ok(content.clone()),
    }];
    let (shaped, provider_bytes) =
        shape_tool_output_batch(&engine, &session, &spec, &project, 0, pending)
            .await
            .unwrap();
    let output = &shaped[0].output;
    let handle = output.output_ref.clone().expect("spilled handle");
    assert!(output.truncated);
    assert!(output.total_bytes >= content.len() as u64);
    assert!(output.content.len() < 5_000);
    assert!(output.content.contains("read_tool_output handle="));
    assert!(provider_bytes <= 64 * 1024);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.output_spilled"
            && event.payload["handle"].as_str() == Some(handle.as_str())
            && event.payload.get("content").is_none()
    }));
    assert!(
        events
            .iter()
            .any(|event| event.kind == "tool.output_truncated")
    );
    let page = run_read_tool_output(
        &engine,
        &session,
        serde_json::json!({
            "handle": handle,
            "offset": 500,
            "limit": 5,
        }),
    )
    .await;
    assert!(!page.is_error);
    assert!(page.content.contains("line 0500"));
    assert!(page.content.contains("line 0504"));
    assert!(page.content.contains("read_tool_output"));
    let foreign = engine
        .create_session(project.id, "foreign".into(), None)
        .await
        .unwrap();
    let denied =
        run_read_tool_output(&engine, &foreign, serde_json::json!({"handle": handle})).await;
    assert!(denied.is_error);
    assert!(denied.content.contains("unknown tool output handle"));
}

#[test]
fn token_aware_cut_keeps_one_oversized_message() {
    let conversation = vec![
        ChatMessage::user("x".repeat(4_000)),
        ChatMessage {
            role: "assistant".into(),
            content: "y".repeat(4_000),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage::user("small tail"),
    ];
    let cut = safe_context_cut_with_tokens(&conversation, 8, 10, 0);
    assert_eq!(cut, None, "there is no safe cut that leaves a usable tail");
    let conversation = vec![
        ChatMessage::user("x".repeat(4_000)),
        ChatMessage {
            role: "assistant".into(),
            content: "y".repeat(4_000),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage::user("tail"),
        ChatMessage {
            role: "assistant".into(),
            content: "last".into(),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    let cut = safe_context_cut_with_tokens(&conversation, 2, 5, 0).unwrap();
    assert_eq!(cut, 2);
    assert_eq!(conversation[cut].role, "user");
}

#[test]
fn zero_keep_tokens_falls_back_to_message_count() {
    let conversation = (0..10)
        .map(|index| {
            if index % 2 == 0 {
                ChatMessage::user(format!("message {index}"))
            } else {
                ChatMessage {
                    role: "assistant".into(),
                    content: format!("message {index}"),
                    tool_calls: None,
                    tool_call_id: None,
                }
            }
        })
        .collect::<Vec<_>>();
    let with_zero = safe_context_cut_with_tokens(&conversation, 4, 0, 0).unwrap();
    let with_tokens = safe_context_cut_with_tokens(&conversation, 4, 1_000_000, 0).unwrap();
    assert_eq!(with_zero, with_tokens);
    assert!(with_zero >= 6);
}

#[tokio::test]
async fn prepare_failure_is_distinct_from_compaction_failure() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![Err(ProviderError::Stream(
            "prepare unavailable".into(),
        ))]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "prepare failure".into(), None)
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_after_tokens = 1_000;
    config.compact_keep_messages = 2;
    config.compact_keep_tokens = 100_000;
    config.compact_prepare_ratio = 0.6;
    config.context_window = 100_000;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
    for index in 0..7 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"x".repeat(400),
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
    let mut runtime = CompactionRuntime::default();
    let changed = compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(!changed);
    assert!(runtime.prepared.is_none());
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "context.compaction_prepare_failed")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "context.compact_failed")
    );
}

#[tokio::test]
async fn compaction_metrics_event_omits_summary_text() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("metrics summary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 0, 3);
    let session = engine
        .create_session(project.id, "metrics".into(), None)
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
                &format!("message {index}: metrics event content"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);
    let events = engine.list_events(session.id, 0, 200).await.unwrap();
    let metrics = events
        .iter()
        .find(|event| event.kind == "context.compaction")
        .expect("context.compaction");
    assert!(metrics.payload.get("summary").is_none());
    assert_eq!(metrics.payload["mode"], "summary");
    assert!(metrics.payload["latency_ms"].as_u64().is_some());
    assert!(metrics.payload["estimated_tokens_before"].as_u64().unwrap() > 0);
    assert!(metrics.payload["estimated_tokens_after"].as_u64().unwrap() > 0);
    assert_eq!(metrics.payload["compaction_count"], 1);
    assert_eq!(metrics.payload["background_reuse"], false);
}

#[tokio::test]
async fn prepared_summary_is_invalidated_by_a_real_plan_revision() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("prepared revision one".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("synchronous revision two".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "plan invalidation".into(), None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_after_tokens = 1_000;
    config.compact_keep_messages = 2;
    config.compact_keep_tokens = 100_000;
    config.compact_prepare_ratio = 0.6;
    config.context_window = 100_000;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
    for index in 0..7 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"x".repeat(400),
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
    let mut runtime = CompactionRuntime::default();
    compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(runtime.prepared.is_some());
    let mut revised = continuation_test_plan();
    revised.goal = "revised plan".into();
    engine.set_team_plan(&session.id, &revised).await.unwrap();
    for index in 7..12 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"y".repeat(400),
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
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(changed);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let compaction = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted");
    assert_eq!(compaction.payload["background_reuse"], false);
    assert_eq!(compaction.payload["fallback_to_sync"], true);
    assert_eq!(compaction.payload["structured_state"]["plan_revision"], 2);
    assert!(
        compaction.payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("synchronous revision two")
                && summary.contains("Goal: revised plan"))
    );
    let summarized = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.system.as_deref() == Some(COMPACTION_SYSTEM_PROMPT))
        .count();
    assert_eq!(summarized, 2);
}

#[tokio::test]
async fn compaction_fallback_preserves_the_authoritative_plan_state() {
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
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_keep_messages = 2;
    config.compact_keep_tokens = 100_000;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        200,
        0,
        1.0,
        true,
        "test-prefix",
        0.6,
        &mut CompactionRuntime::default(),
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(changed);
    assert!(
        conversation[0].content.contains(STRUCTURED_STATE_START),
        "structured state must survive the trim fallback: {}",
        conversation[0].content
    );
    assert!(
        conversation[0]
            .content
            .contains("Goal: deferred continuation")
    );
    assert!(conversation[0].content.contains("T1"));
    assert!(conversation[0].content.contains("src/one"));
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let compaction = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted");
    assert_eq!(compaction.payload["mode"], "trim");
    assert!(compaction.payload["structured_state"]["active_task_id"] == "T1");
    assert!(compaction.payload["latency_ms"].as_u64().is_some());
    assert!(
        events
            .iter()
            .any(|event| event.kind == "context.compaction")
    );
}

#[tokio::test]
async fn prepared_summary_is_reused_once_at_the_threshold() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("prepared summary text".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "prepare context".into(), None)
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_after_tokens = 1_000;
    config.compact_keep_messages = 2;
    config.compact_keep_tokens = 100_000;
    config.compact_prepare_ratio = 0.6;
    config.context_window = 100_000;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
    for index in 0..7 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"x".repeat(400),
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
    let mut runtime = CompactionRuntime::default();
    let changed = compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(!changed, "preparation must not swap the summary");
    assert!(runtime.prepared.is_some());
    for index in 7..12 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"y".repeat(400),
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
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(changed);
    assert!(runtime.prepared.is_none());
    assert!(
        conversation
            .iter()
            .any(|message| message.content.starts_with('y')),
        "messages appended after the prepared cut must remain in the tail"
    );
    let summarized = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.system.as_deref() == Some(COMPACTION_SYSTEM_PROMPT))
        .count();
    assert_eq!(summarized, 1, "prepared summary must avoid a second call");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let compaction = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted");
    assert_eq!(compaction.payload["background_reuse"], true);
    assert_eq!(compaction.payload["compaction_count"], 1);
    assert!(
        compaction.payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("prepared summary text")
                && summary.contains(STRUCTURED_STATE_START))
    );
}

#[tokio::test]
async fn prepared_summary_is_invalidated_by_model_and_plan_changes() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("prepared summary text".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("synchronous summary text".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "invalidate context".into(), None)
        .await
        .unwrap();
    let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
    config.compact_after_tokens = 1_000;
    config.compact_keep_messages = 2;
    config.compact_keep_tokens = 100_000;
    config.compact_prepare_ratio = 0.6;
    config.context_window = 100_000;
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
    for index in 0..7 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"x".repeat(400),
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
    let mut runtime = CompactionRuntime::default();
    compact_context_if_needed(
        &engine,
        &config,
        &session,
        &spec,
        &budget,
        &control,
        &mut conversation,
        &mut conversation_ids,
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(runtime.prepared.is_some());
    config.model = "different-model".into();
    for index in 7..12 {
        let message = engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &"y".repeat(400),
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
        1_000,
        100_000,
        0,
        1.0,
        false,
        "stable-prefix",
        0.6,
        &mut runtime,
        ContextStrategy::Summarize,
    )
    .await
    .unwrap();
    assert!(changed);
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let compaction = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted");
    assert_eq!(compaction.payload["background_reuse"], false);
    assert_eq!(compaction.payload["fallback_to_sync"], true);
    assert!(
        compaction.payload["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("synchronous summary text"))
    );
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
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
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
    let structured = compaction.structured_state.expect("structured state");
    assert_eq!(structured.active_task_id.as_deref(), Some("T1"));
    assert_eq!(structured.goal, "deferred continuation");
    assert_eq!(structured.plan_revision, 1);

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
    assert!(loaded.messages[0].content.contains(STRUCTURED_STATE_START));
    assert!(
        loaded.messages[0]
            .content
            .contains("Goal: deferred continuation")
    );
    assert!(loaded.messages[0].content.contains("T1"));
    let status = engine.session_context_status(&session.id).await.unwrap();
    assert_eq!(status.compaction_count, 1);
    assert!(status.last_compaction_latency_ms < 60_000);
    assert_eq!(
        status.last_compaction_tokens_before,
        compaction.estimated_tokens_before
    );
    assert_eq!(
        status.last_compaction_tokens_after,
        compaction.estimated_tokens_after
    );
    let usage = status.usage.expect("post-compaction usage");
    let summary = status.last_compaction.expect("last compaction");
    assert_eq!(usage.estimated_tokens, summary.estimated_tokens_after);
    assert!(usage.estimated_tokens < summary.estimated_tokens_before);
}

#[tokio::test]
async fn manual_compaction_count_and_latency_are_cumulative() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("first summary".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second summary".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 0, 3);
    let session = engine
        .create_session(project.id, "count context".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..14 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: cumulative compaction metrics"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    let first = service.compact_now(&session.id).await.unwrap();
    assert_eq!(first.compaction.as_ref().unwrap().compaction_count, 1);
    // Add tail messages so the second pass has work even after the first
    // summary was inserted.
    for index in 14..22 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: second compaction window"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    let second = service.compact_now(&session.id).await.unwrap();
    assert!(second.compacted);
    assert_eq!(second.compaction.as_ref().unwrap().compaction_count, 2);
    assert!(!second.compaction.as_ref().unwrap().background_reuse);
    let status = engine.session_context_status(&session.id).await.unwrap();
    assert_eq!(status.compaction_count, 2);
    let compactions = engine
        .list_events(session.id, 0, 300)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "context.compacted")
        .count();
    assert_eq!(compactions, 2);
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

    // A failed summary falls back to a rolling trim, so the control is still
    // useful and the run registry is released. The second call gets the
    // recovered summary from the provider.
    let first = service.compact_now(&session.id).await.unwrap();
    assert!(first.compacted);
    assert_eq!(
        first.compaction.map(|summary| summary.mode),
        Some("trim".into())
    );
    // The registry is released: a second call proceeds instead of reporting
    // that the session is already running. It may be a no-op when the trim
    // already left fewer than the retained tail.
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted || result.compaction.is_none());
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
    let (threshold_128k, hard_128k) = context_limits(&config, AgentRole::Leader);
    assert!(
        threshold_128k > 110_000,
        "128k model threshold must follow the 90% window policy"
    );
    assert!(hard_128k > threshold_128k);
    assert_ne!(
        threshold_128k,
        hard_128k.saturating_mul(3) / 4,
        "the old 75% threshold is gone"
    );

    config.context_window = 1_000_000;
    let (threshold_1m, hard_1m) = context_limits(&config, AgentRole::Leader);
    assert!(
        threshold_1m > 890_000,
        "1M DeepSeek window must not compact at 75%"
    );
    assert!(hard_1m > threshold_1m);
    assert_ne!(threshold_1m, 750_000);

    config.context_window = 8_000;
    config.max_tokens = Some(2_048);
    let (threshold_8k, hard_8k) = context_limits(&config, AgentRole::Leader);
    assert_eq!(
        threshold_8k, hard_8k,
        "a small window is bounded by the hard output reserve"
    );
    assert!(
        threshold_8k > 4_000,
        "8k threshold must not use the old 3k default"
    );
    assert!(hard_8k < 5_000);

    // An explicit override remains explicit and is only clamped to the hard
    // limit; it never silently becomes the 90% default.
    config.compact_after_tokens = 4_000;
    let (override_threshold, hard_override) = context_limits(&config, AgentRole::Leader);
    assert_eq!(override_threshold, 4_000);
    assert_eq!(hard_override, hard_8k);

    config.context_window = 8_000;
    config.role_models.worker.context_window = Some(200_000);
    assert!(
        context_window_for(&config, AgentRole::Worker)
            > context_window_for(&config, AgentRole::Leader)
    );

    config.context_window = 0;
    let (threshold_off, hard_off) = context_limits(&config, AgentRole::Leader);
    assert_eq!(
        threshold_off, 4_000,
        "an explicit override survives a disabled automatic window"
    );
    assert_eq!(hard_off, u64::MAX);
    config.compact_after_tokens = 0;
    let (threshold_disabled, hard_disabled) = context_limits(&config, AgentRole::Leader);
    assert_eq!(threshold_disabled, 0);
    assert_eq!(hard_disabled, u64::MAX);
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
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        "test-prefix",
        0.6,
        &mut super::CompactionRuntime::default(),
        super::ContextStrategy::Summarize,
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
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        "test-prefix",
        0.6,
        &mut super::CompactionRuntime::default(),
        super::ContextStrategy::Summarize,
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
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from("."),
        branch: "test".into(),
        session_worktree: PathBuf::from("."),
        session_branch: "test".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
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
        "test-prefix",
        0.6,
        &mut super::CompactionRuntime::default(),
        super::ContextStrategy::Trim,
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
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second answer".into()),
                StreamEvent::Usage {
                    input_tokens: 20_000,
                    output_tokens: 10,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
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

#[test]
fn context_strategy_prefers_summary_for_leaders_and_trim_for_workers() {
    assert_eq!(
        super::context_strategy(super::AgentRole::Leader),
        super::ContextStrategy::Summarize
    );
    assert_eq!(
        super::context_strategy(super::AgentRole::Manager),
        super::ContextStrategy::Summarize
    );
    assert_eq!(
        super::context_strategy(super::AgentRole::Worker),
        super::ContextStrategy::Trim
    );
    assert_eq!(
        super::context_strategy(super::AgentRole::Labour),
        super::ContextStrategy::Trim
    );
    assert_eq!(
        super::context_strategy(super::AgentRole::Verifier),
        super::ContextStrategy::Trim
    );
}

#[tokio::test]
async fn completed_task_results_become_persistent_project_memory() {
    let data = tempfile::TempDir::new().unwrap();
    let repo = tempfile::TempDir::new().unwrap();
    std::fs::write(repo.path().join("README.md"), "memory\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@verlinpark.local"],
        vec!["config", "user.name", "VerlinPark Test"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let engine = Engine::new(vp_store::Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "remember work".into(), None)
        .await
        .unwrap();
    super::record_task_memory(
        &engine,
        &session.id,
        "T1",
        "Implemented the parser and tests.",
    )
    .await;
    // Identical content is idempotent; a retried task with different content
    // supersedes the previous active row (covered by the evidence test).
    super::record_task_memory(
        &engine,
        &session.id,
        "T1",
        "Implemented the parser and tests.",
    )
    .await;
    let memories = engine
        .list_memories(
            &project.id,
            Some("Task T1 result".into()),
            Some("task_result".into()),
            10,
        )
        .await
        .unwrap()
        .memories;
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].title, "Task T1 result");
    assert!(memories[0].body.contains("Implemented the parser"));
    assert!(memories[0].tags.contains(&"T1".to_string()));
}

#[test]
fn blocked_worker_reports_are_not_successful_task_completions() {
    assert!(super::worker_report_reports_failure(
        "## N1 — BLOCKED (task not completed; no write tool)"
    ));
    assert!(super::worker_report_reports_failure(
        "permission denied while editing"
    ));
    assert!(!super::worker_report_reports_failure(
        "No changes were required; the existing implementation already satisfies the task."
    ));
}

#[tokio::test]
async fn dispatches_an_intake_task_through_worker_and_verifier() {
    use vp_protocol::types::IntakeCreateParams;

    fn write_task(path: &str, content: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCall(ToolCall {
                id: "write_intake".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": path, "content": content}).to_string(),
            }),
            StreamEvent::Done,
        ]
    }
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            write_task("src/intake.txt", "standalone work\n"),
            vec![
                StreamEvent::TextDelta("standalone work done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "verdict_intake".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "the intake task is complete"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
        ])),
    });
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .set_task_scheduler_policy(
            &project.id,
            vp_protocol::types::TaskSchedulerPolicy {
                enabled: false,
                cooldown_secs: 60,
                dispatch_tasks: false,
                include_intake: false,
                max_intake_parallel: 1,
            },
        )
        .await
        .unwrap();
    let task = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Standalone change".into(),
            instructions: "Create src/intake.txt and verify it.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: Some(15),
            write_scope: vec!["src/intake.txt".into()],
            acceptance: vec!["the file contains standalone work".into()],
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: vec!["test".into()],
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    let dispatched = service.dispatch_intake_task(&task.id).await.unwrap();
    assert!(dispatched.started);
    let session_id = dispatched.session_id.clone().unwrap();
    let sessions = engine
        .list_sessions(Some(project.id.clone()))
        .await
        .unwrap();
    let session = sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert!(matches!(session.status.as_str(), "queued" | "running"));

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let finished = loop {
        let task = engine
            .get_intake_task(task.id.clone())
            .await
            .unwrap()
            .unwrap();
        if task.status == "running" {
            let _ = service.reconcile_intake_tasks().await;
            let current = engine
                .get_intake_task(task.id.clone())
                .await
                .unwrap()
                .unwrap();
            if current.status != "running" {
                break current;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "intake task did not settle"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(finished.task_id.as_deref(), Some("T1"));
    assert_eq!(finished.attempts, 1);
    assert_eq!(finished.profile, "coding");
    let plan = engine.get_team_plan(&session_id).await.unwrap().unwrap();
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.tasks[0].write_scope, vec!["src/intake.txt"]);
    let events = engine
        .list_events(session_id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "intake.dispatched"));
    assert!(events.iter().any(|event| event.kind == "intake.completed"));
    let files = engine
        .workspace_file(&session_id, "src/intake.txt")
        .await
        .unwrap();
    assert_eq!(files.content, "standalone work\n");
}

#[tokio::test]
async fn cancels_retries_and_deletes_an_intake_task() {
    use vp_protocol::types::IntakeCreateParams;

    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let task = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Slow research".into(),
            instructions: "Take a long time.".into(),
            profile: Some("research".into()),
            kind: Some("research".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    let dispatched = service.dispatch_intake_task(&task.id).await.unwrap();
    assert!(dispatched.started);
    let session_id = dispatched.session_id.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let sessions = engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap();
        let session = sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .unwrap();
        if session.status == "running" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session did not start: {}",
            session.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let cancelled = service.cancel_intake_task(&task.id).await.unwrap();
    assert_eq!(cancelled.status, "cancelled");
    let retried = service.retry_intake_task(&task.id).await.unwrap();
    assert_eq!(retried.status, "queued");
    assert_eq!(retried.attempts, 0);
    assert!(retried.last_error.is_none());
    service.delete_intake_task(&task.id).await.unwrap();
    assert!(engine.get_intake_task(task.id).await.unwrap().is_none());
}

#[tokio::test]
async fn scheduler_status_reports_plan_and_intake_occupancy() {
    use vp_protocol::types::IntakeCreateParams;

    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "status".into(), None)
        .await
        .unwrap();
    let plan: TeamPlan = serde_json::from_value(serde_json::json!({
        "goal": "status",
        "departments": [{
            "name": "impl",
            "manager_title": "Implementation",
            "profile": "coding",
            "objective": "build"
        }],
        "tasks": [{
            "id": "T1",
            "department": "impl",
            "title": "One",
            "kind": "coding",
            "deps": [],
            "write_scope": ["src/t1"],
            "acceptance": ["done"]
        }]
    }))
    .unwrap();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Intake".into(),
            instructions: "Queue one.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: Some(3),
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: None,
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();

    let status = service
        .scheduler_status(Some(project.id.clone()))
        .await
        .unwrap();
    assert_eq!(status.pending_plan_tasks, 1);
    assert_eq!(status.running_plan_tasks, 0);
    assert_eq!(status.queued_intake, 1);
    assert_eq!(status.active_intake, 0);
    assert_eq!(status.projects.len(), 1);
    let row = &status.projects[0];
    assert_eq!(row.project_id, project.id);
    assert_eq!(row.pending_sessions, 1);
    assert!(row.enabled);
    assert!(row.include_intake);
    assert_eq!(row.max_intake_parallel, 2);
}

#[tokio::test]
async fn cancelling_a_dependency_blocks_dependents_until_retried() {
    use vp_protocol::types::IntakeCreateParams;

    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let parent = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Parent".into(),
            instructions: "Parent work.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    let child = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Child".into(),
            instructions: "Child work.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: vec![parent.id.clone()],
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    service.cancel_intake_task(&parent.id).await.unwrap();
    let child_after = engine
        .get_intake_task(child.id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_after.status, "blocked");
    assert!(
        child_after
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("cancelled")
    );
    assert!(service.delete_intake_task(&parent.id).await.is_err());
    let retried = service.retry_intake_task(&child.id).await.unwrap();
    assert_eq!(retried.status, "queued");
    service.cancel_intake_task(&child.id).await.unwrap();
    assert!(service.delete_intake_task(&parent.id).await.is_ok());
}

#[tokio::test]
async fn dependency_reconciler_blocks_after_permanent_failure() {
    use vp_protocol::types::IntakeCreateParams;

    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let parent = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Permanent failure".into(),
            instructions: "Fail.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(1),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    let child = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Dependent".into(),
            instructions: "Wait.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: vec![parent.id.clone()],
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    engine
        .claim_intake_task(parent.id.clone())
        .await
        .unwrap()
        .unwrap();
    engine
        .settle_intake_task(
            parent.id.clone(),
            "failed".to_string(),
            None,
            Some("boom".to_string()),
            None,
        )
        .await
        .unwrap();
    let settled = service.reconcile_intake_tasks().await.unwrap();
    assert!(settled >= 1);
    let child = engine.get_intake_task(child.id).await.unwrap().unwrap();
    assert_eq!(child.status, "blocked");
    assert!(
        child
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("failed")
    );
}

#[tokio::test]
async fn dependency_reconciler_waits_for_available_automatic_retry() {
    use vp_protocol::types::IntakeCreateParams;

    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let parent = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Retryable".into(),
            instructions: "Try twice.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    let child = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Waits".into(),
            instructions: "Wait for retry.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: vec![parent.id.clone()],
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: Some(2),
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    engine
        .claim_intake_task(parent.id.clone())
        .await
        .unwrap()
        .unwrap();
    engine
        .settle_intake_task(
            parent.id.clone(),
            "failed".to_string(),
            None,
            Some("will retry".to_string()),
            None,
        )
        .await
        .unwrap();
    service.reconcile_intake_tasks().await.unwrap();
    let child = engine.get_intake_task(child.id).await.unwrap().unwrap();
    assert_eq!(child.status, "queued");
}

#[tokio::test]
async fn source_search_attaches_citation_evidence_to_agent_messages() {
    use vp_protocol::types::SourceCreateNoteParams;

    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "search_sources_1".into(),
                    name: "search_sources".into(),
                    arguments: serde_json::json!({
                        "query": "deterministic leases",
                        "limit": 4
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("The scheduler uses deterministic leases [c1].".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let source = engine
        .add_source_note(SourceCreateNoteParams {
            project_id: project.id.clone(),
            collection: None,
            title: "Scheduler notes".into(),
            content: "The scheduler uses deterministic leases for integration.".into(),
            labels: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(source.status, "indexed");
    let session = engine
        .create_session(project.id.clone(), "answer from sources".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "answer from sources")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let message = events
        .iter()
        .find(|event| {
            event.kind == "agent.message"
                && event.payload["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("[c1]"))
        })
        .expect("final assistant message");
    assert!(message.payload["message_id"].as_str().is_some());
    let citations = message.payload["citations"]
        .as_array()
        .expect("citations array");
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["key"], "c1");
    assert_eq!(citations[0]["source_id"], source.id);
    assert_eq!(citations[0]["origin"], "search");
    assert!(
        citations[0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("deterministic leases")
    );
    let persisted = engine.session_citations(&session.id, None).await.unwrap();
    assert_eq!(persisted.citations.len(), 1);
    assert_eq!(persisted.citations[0].citation.key, "c1");
    assert_eq!(persisted.citations[0].citation.source_id, source.id);
    let audit = engine
        .session_citation_audit(&session.id)
        .await
        .unwrap()
        .audit;
    assert_eq!(audit.messages_with_citations, 1);
    assert_eq!(audit.messages_without_citations, 0);
    assert_eq!(audit.total_citations, 1);
    assert_eq!(audit.unique_sources, 1);
    assert_eq!(audit.dangling_key_count, 0);
    assert_eq!(audit.unused_citation_count, 0);
}

#[test]
fn skill_tool_wildcards_narrow_without_granting() {
    let mut skill = Skill {
        id: "skill-wildcard".into(),
        project_id: "project".into(),
        name: "Wildcard".into(),
        slug: "wildcard".into(),
        description: "Test wildcard policy".into(),
        when_to_use: String::new(),
        version: "1.0.0".into(),
        instructions: "Test".into(),
        tool_allow: Vec::new(),
        tool_deny: vec!["*".into()],
        profiles: Vec::new(),
        roles: Vec::new(),
        enabled: true,
        source: "test".into(),
        sha256: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    assert!(!skill_allows_tool(&skill, "read_file"));
    assert!(!skill_allows_tool(&skill, "shell"));
    assert!(skill_allows_tool(&skill, "use_skill"));
    skill.tool_deny.clear();
    skill.tool_allow = vec!["*".into()];
    assert!(skill_allows_tool(&skill, "read_file"));
    skill.tool_allow = vec!["read_file".into()];
    assert!(skill_allows_tool(&skill, "read_file"));
    assert!(!skill_allows_tool(&skill, "shell"));
}

#[tokio::test]
async fn active_skill_narrows_tools_at_execution_time() {
    use vp_protocol::types::SkillCreateParams;

    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "use_skill_1".into(),
                    name: "use_skill".into(),
                    arguments: serde_json::json!({"name": "Read Only"}).to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "write_blocked".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "blocked.txt",
                        "content": "should not be written"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("The skill blocked the write.".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .create_skill(SkillCreateParams {
            project_id: project.id.clone(),
            name: "Read Only".into(),
            description: "Only inspect files.".into(),
            when_to_use: "When editing is forbidden.".into(),
            version: None,
            instructions: "Do not write files.".into(),
            tool_allow: vec!["read_file".into()],
            tool_deny: Vec::new(),
            profiles: Vec::new(),
            roles: vec!["leader".into()],
        })
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "skill policy".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "skill policy").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let blocked = events
        .iter()
        .find(|event| {
            event.kind == "tool.finished" && event.payload["tool_call_id"] == "write_blocked"
        })
        .expect("blocked tool event");
    assert!(
        blocked.payload["output"]
            .as_str()
            .unwrap()
            .contains("not allowed by active skill")
    );
    assert!(
        engine
            .workspace_file(&session.id, "blocked.txt")
            .await
            .is_err()
    );
    let activations = engine.session_skill_activations(&session.id).await.unwrap();
    assert_eq!(activations.activations.len(), 1);
    assert_eq!(activations.activations[0].name, "Read Only");
    assert_eq!(activations.activations[0].role, "leader");
    assert_eq!(activations.activations[0].tool_allow, vec!["read_file"]);
    assert!(activations.activations[0].agent_name.is_some());
    assert!(activations.activations[0].agent_id.is_some());
    assert!(!activations.activations[0].activated_at.is_empty());

    // Activating a skill must not change the advertised tool bundle. Policy
    // enforcement happens when the disallowed call is dispatched.
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 3);
    let expected = recorded[0]
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    assert!(expected.contains(&"write_file".to_string()));
    let expected_hash = prompt_prefix_fingerprint("test", &recorded[0].tools, "").tool_bundle_hash;
    for request in recorded.iter() {
        let names = request
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
        assert_eq!(
            prompt_prefix_fingerprint("test", &request.tools, "").tool_bundle_hash,
            expected_hash
        );
    }
}

#[tokio::test]
async fn stable_system_prefix_precedes_run_context_and_is_hashed() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id.clone(), "cache the prefix".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    service
        .start(&session.id, "cache the prefix")
        .await
        .unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    {
        let recorded = requests.lock().unwrap();
        let request = recorded.first().expect("provider request");
        let system = request.system.as_deref().expect("system prompt");
        let stable = request
            .system_stable_prefix
            .as_deref()
            .expect("stable prefix marker");
        assert!(system.starts_with(stable));
        let security = system.find("Security and output contract:").unwrap();
        let goal = system.find("Session goal:").unwrap();
        let worktree = system.find("Worktree:").unwrap();
        assert!(security < goal);
        assert!(security < worktree);
        assert!(!stable.contains("Session goal:"));
        assert!(!stable.contains("Worktree:"));
    }

    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix event");
    assert_eq!(
        prefix.payload["version"].as_u64(),
        Some(u64::from(crate::prompt_cache::PROMPT_SECTION_VERSION))
    );
    assert_eq!(prefix.payload["model"], "test");
    assert!(
        prefix.payload["tool_bundle_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
    );
    for field in [
        "core_hash",
        "project_hash",
        "tools_hash",
        "volatile_hash",
        "prefix_hash",
        "cache_generation",
    ] {
        assert!(
            prefix.payload[field]
                .as_str()
                .is_some_and(|hash| hash.len() == 64),
            "missing 64-char hash for {field}"
        );
    }
    assert_eq!(
        prefix.payload["cache_generation"],
        prefix.payload["prefix_hash"]
    );
}

#[tokio::test]
async fn stable_prompt_prefix_is_shared_across_sessions() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
            vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let first = engine
        .create_session(project.id.clone(), "first session".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&first.id, 2)
        .await
        .unwrap();
    service.start(&first.id, "first session").await.unwrap();
    let _ = wait_until_idle(&engine, &first).await;
    let second = engine
        .create_session(project.id.clone(), "second session".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&second.id, 2)
        .await
        .unwrap();
    service.start(&second.id, "second session").await.unwrap();
    let _ = wait_until_idle(&engine, &second).await;

    let prefix_hash = |events: Vec<vp_protocol::types::Event>| {
        events
            .into_iter()
            .find(|event| event.kind == "context.prompt_prefix")
            .and_then(|event| event.payload["prefix_hash"].as_str().map(str::to_string))
            .expect("prompt prefix event")
    };
    let first_events = engine.list_events(first.id.clone(), 0, 100).await.unwrap();
    let second_events = engine.list_events(second.id.clone(), 0, 100).await.unwrap();
    assert_eq!(prefix_hash(first_events), prefix_hash(second_events));

    let recorded = requests.lock().unwrap();
    let first_stable = recorded[0].system_stable_prefix.as_deref().unwrap();
    let second_stable = recorded[1].system_stable_prefix.as_deref().unwrap();
    assert_eq!(first_stable, second_stable);
    assert_ne!(recorded[0].system, recorded[1].system);
}

fn set_leader_prices(
    service: &AgentService,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.role_models.leader.input_usd_per_million = Some(input);
    config.role_models.leader.output_usd_per_million = Some(output);
    config.role_models.leader.cache_read_usd_per_million = Some(cache_read);
    config.role_models.leader.cache_write_usd_per_million = Some(cache_write);
    *service.config.write().unwrap() = Some(config);
}

fn set_worker_model_and_prices(
    service: &AgentService,
    model: &str,
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.role_models.worker.model = Some(model.to_string());
    config.role_models.worker.input_usd_per_million = Some(input);
    config.role_models.worker.output_usd_per_million = Some(output);
    config.role_models.worker.cache_read_usd_per_million = Some(cache_read);
    config.role_models.worker.cache_write_usd_per_million = Some(cache_write);
    *service.config.write().unwrap() = Some(config);
}

#[tokio::test]
async fn prompt_sections_keep_core_project_and_volatile_boundaries() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .create_skill(vp_protocol::types::SkillCreateParams {
            project_id: project.id.clone(),
            name: "Boundary Skill".into(),
            description: "Exercise the project section.".into(),
            when_to_use: "When testing cache boundaries.".into(),
            version: None,
            instructions: "Keep sections stable.".into(),
            tool_allow: Vec::new(),
            tool_deny: Vec::new(),
            profiles: Vec::new(),
            roles: vec!["leader".into()],
        })
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "section boundaries".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .create_memory(
            &session.id,
            "fact",
            "Boundary fact",
            "This memory belongs in the volatile section.",
            vec![],
        )
        .await
        .unwrap();
    service
        .start(&session.id, "section boundaries")
        .await
        .unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    {
        let recorded = requests.lock().unwrap();
        assert_eq!(
            recorded.len(),
            1,
            "default warm_on_resume must not add a call"
        );
        let request = &recorded[0];
        let sections = &request.system_sections;
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].kind, "core");
        assert_eq!(sections[1].kind, "project");
        assert_eq!(sections[2].kind, "volatile");
        assert!(sections[0].text.contains("Role charter:"));
        assert!(!sections[0].text.contains("Session goal:"));
        assert!(!sections[0].text.contains("Worktree:"));
        assert!(!sections[0].text.contains("Run memory"));
        assert!(sections[1].text.contains("Boundary Skill"));
        assert!(!sections[1].text.contains("Session goal:"));
        assert!(sections[2].text.contains("Session goal:"));
        assert!(sections[2].text.contains("Worktree:"));
        let stable = request.system_stable_prefix.as_deref().unwrap();
        assert!(stable.starts_with(&sections[0].text));
        assert!(stable.contains(&sections[1].text));
        assert!(!stable.contains("Session goal:"));
        assert!(!stable.contains("Worktree:"));
        let combined = request.system.as_deref().unwrap();
        assert!(combined.starts_with(&sections[0].text));
        assert!(combined.ends_with(&sections[2].text));
        assert!(
            combined.find("Role charter:").unwrap() < combined.find("Session goal:").unwrap(),
            "stable core must precede volatile run context"
        );
        let cache = request.cache.expect("cache policy is enabled by default");
        assert!(cache.enabled);
        assert_eq!(cache.ttl.as_str(), "5m");
        assert!(cache.automatic);
    }
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix");
    assert!(prefix.payload["core_hash"].as_str().is_some());
    assert!(prefix.payload["project_hash"].as_str().is_some());
    assert!(prefix.payload["tools_hash"].as_str().is_some());
    assert!(prefix.payload["volatile_hash"].as_str().is_some());
    assert_eq!(prefix.payload["cache_policy"]["ttl"], "5m");
    assert_eq!(prefix.payload["cache_plan"]["provider"], "queue");
    assert_eq!(prefix.payload["cache_plan"]["breakpoints"], 0);
    let cache_event = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache_event.payload["miss_reason"], "first_call");
    assert_eq!(cache_event.payload["cache_ttl"], "5m");
    assert_eq!(cache_event.payload["cache_breakpoints"], 0);
    assert_eq!(cache_event.payload["warm"], false);
    assert!(cache_event.payload["core_hash"].as_str().is_some());
    assert!(cache_event.payload["prompt_prefix_hash"].as_str().is_some());
}

#[tokio::test]
async fn cache_telemetry_prices_billed_reads_and_derives_first_call() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("priced".into()),
            StreamEvent::Usage {
                input_tokens: 1_000,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 100,
            },
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_leader_prices(&service, 3.0, 15.0, 0.3, 3.75);
    let session = engine
        .create_session(project.id, "price the cache".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "price the cache").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let cache_event = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache_event.payload["input_tokens"], 1_000);
    assert_eq!(cache_event.payload["cache_read_tokens"], 0);
    assert_eq!(cache_event.payload["cache_write_tokens"], 100);
    assert_eq!(cache_event.payload["miss_reason"], "first_call");
    assert_eq!(cache_event.payload["cache_write_ratio"], 0.1);
    let savings = cache_event.payload["estimated_savings_usd"]
        .as_f64()
        .expect("savings");
    assert!(savings < 0.0, "cache writes can outweigh reads: {savings}");
    assert!((savings + 0.000_075).abs() < 1e-9, "{savings}");

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.input_tokens, 1_000);
    assert_eq!(status.cache_write_tokens, 100);
    assert_eq!(status.cache_calls, 1);
    assert_eq!(status.recent_calls.len(), 1);
    assert_eq!(status.last_miss_reason.as_deref(), Some("first_call"));
    assert_eq!(status.last_breakpoints, 0);
    assert_eq!(status.last_cache_ttl.as_deref(), Some("5m"));
    assert!((status.estimated_savings_usd - savings).abs() < 1e-9);
    assert!((status.cache_write_ratio - 0.1).abs() < 1e-9);
    assert!(status.policy.as_ref().is_some_and(|policy| policy.enabled));
    assert_eq!(
        status.recent_calls[0].prompt_prefix_hash.as_deref(),
        cache_event.payload["prompt_prefix_hash"].as_str()
    );
    assert_eq!(status.miss_reason_counts.len(), 1);
    assert_eq!(status.miss_reason_counts[0].key, "first_call");
    assert_eq!(status.miss_reason_counts[0].calls, 1);
    assert_eq!(status.ttl_counts.len(), 1);
    assert_eq!(status.ttl_counts[0].key, "5m");
    assert_eq!(status.breakpoint_counts.len(), 1);
    assert_eq!(status.breakpoint_counts[0].key, "0");
    assert_eq!(status.savings_by_role.len(), 1);
    assert_eq!(status.savings_by_role[0].role, "leader");
    assert!((status.savings_by_role[0].estimated_savings_usd - savings).abs() < 1e-9);
}

#[tokio::test]
async fn session_cache_status_keeps_the_last_agent_miss_after_a_warm() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("run".into()),
                StreamEvent::Usage {
                    input_tokens: 1_000,
                    output_tokens: 2,
                    cache_read_tokens: 0,
                    cache_write_tokens: 100,
                },
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("OK".into()),
                StreamEvent::Usage {
                    input_tokens: 500,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 400,
                },
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "warm hides miss".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "warm hides miss").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;
    let before = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(before.last_miss_reason.as_deref(), Some("first_call"));
    service.warm_cache_now(&session.id, None).await.unwrap();
    let after = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(after.last_miss_reason.as_deref(), Some("first_call"));
    assert_eq!(after.recent_calls.first().map(|call| call.warm), Some(true));
    assert_eq!(after.cache_calls, 1);
    assert_eq!(
        after.recent_calls.iter().filter(|call| call.warm).count(),
        1
    );
}

#[tokio::test]
async fn session_cache_status_bounds_recent_calls() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "bounded calls".into(), None)
        .await
        .unwrap();
    for index in 0..30 {
        engine
            .append_event(
                &session.id,
                "context.cache",
                serde_json::json!({
                    "input_tokens": 100 + index,
                    "cache_read_tokens": index,
                    "cache_breakpoints": 4,
                    "cache_ttl": "5m",
                    "prompt_prefix_hash": format!("prefix-{index}")
                }),
            )
            .await
            .unwrap();
    }
    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.cache_calls, 30);
    assert_eq!(status.recent_calls.len(), 20);
    assert_eq!(
        status.recent_calls[0].prompt_prefix_hash.as_deref(),
        Some("prefix-29")
    );
    assert_eq!(
        status.recent_calls[19].prompt_prefix_hash.as_deref(),
        Some("prefix-10")
    );
}

#[tokio::test]
async fn project_content_changes_invalidate_only_the_project_component() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
            vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let first = engine
        .create_session(project.id.clone(), "before skill".into(), None)
        .await
        .unwrap();
    service.start(&first.id, "before skill").await.unwrap();
    let _ = wait_until_idle(&engine, &first).await;

    engine
        .create_skill(vp_protocol::types::SkillCreateParams {
            project_id: project.id.clone(),
            name: "Invalidation Skill".into(),
            description: "Changes the project section.".into(),
            when_to_use: "When testing invalidation scope.".into(),
            version: None,
            instructions: "Do not change core.".into(),
            tool_allow: Vec::new(),
            tool_deny: Vec::new(),
            profiles: Vec::new(),
            roles: vec!["leader".into()],
        })
        .await
        .unwrap();
    let second = engine
        .create_session(project.id.clone(), "after skill".into(), None)
        .await
        .unwrap();
    service.start(&second.id, "after skill").await.unwrap();
    let _ = wait_until_idle(&engine, &second).await;

    let hashes = |events: Vec<vp_protocol::types::Event>| {
        let prefix = events
            .into_iter()
            .find(|event| event.kind == "context.prompt_prefix")
            .expect("prompt prefix");
        (
            prefix.payload["core_hash"].as_str().unwrap().to_string(),
            prefix.payload["project_hash"].as_str().unwrap().to_string(),
            prefix.payload["tools_hash"].as_str().unwrap().to_string(),
            prefix.payload["prefix_hash"].as_str().unwrap().to_string(),
        )
    };
    let before = hashes(engine.list_events(first.id, 0, 100).await.unwrap());
    let after = hashes(engine.list_events(second.id, 0, 100).await.unwrap());
    assert_eq!(before.0, after.0, "core must stay stable");
    assert_eq!(before.2, after.2, "tool bundle must stay stable");
    assert_ne!(before.1, after.1, "skill catalog must move project hash");
    assert_ne!(before.3, after.3, "project change must move the prefix");
}

#[tokio::test]
async fn manual_compaction_records_cache_generation_and_invalidation_estimate() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("cache-aware summary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 0, 3);
    set_leader_prices(&service, 3.0, 15.0, 0.3, 3.75);
    let session = engine
        .create_session(project.id.clone(), "cache aware compaction".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..14 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: compaction cache generation {index}"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);
    let summary = result.compaction.expect("compaction summary");
    assert!(summary.cache_compact_aware);
    assert_eq!(summary.cache_reset_reason.as_deref(), Some("compaction"));
    assert!(
        summary
            .cache_generation_before
            .as_deref()
            .is_some_and(|hash| hash.len() == 64)
    );
    assert!(
        summary
            .cache_generation_after
            .as_deref()
            .is_some_and(|hash| hash.len() == 64)
    );
    assert_ne!(
        summary.cache_generation_before,
        summary.cache_generation_after
    );
    assert!(summary.cache_invalidation_tokens > 0);
    assert!(summary.cache_invalidation_estimated_usd >= 0.0);
    let status = engine.session_context_status(&session.id).await.unwrap();
    let last = status
        .last_compaction
        .expect("persisted compaction summary");
    assert_eq!(
        last.cache_generation_before,
        summary.cache_generation_before
    );
    assert_eq!(last.cache_reset_reason.as_deref(), Some("compaction"));
    assert!(last.cache_compact_aware);
    let event = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.compaction")
        .expect("context.compaction");
    assert!(event.payload.get("summary").is_none());
    assert_eq!(event.payload["cache_reset_reason"], "compaction");
    assert!(event.payload["cache_invalidation_tokens"].as_u64().unwrap() > 0);

    // compact_aware=false still compacts, but it does not claim an
    // invalidation estimate or a reset reason.
    engine
        .set_cache_policy(
            &project.id,
            vp_protocol::types::CachePolicy {
                compact_aware: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    for index in 14..24 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: second cache-aware window"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    let second = service.compact_now(&session.id).await.unwrap();
    assert!(second.compacted);
    let second = second.compaction.expect("second compaction summary");
    assert!(!second.cache_compact_aware);
    assert!(second.cache_reset_reason.is_none());
    assert_eq!(second.cache_invalidation_tokens, 0);
    assert_eq!(second.cache_invalidation_estimated_usd, 0.0);
    assert_ne!(
        second.cache_generation_before,
        second.cache_generation_after
    );
}

#[tokio::test]
async fn cache_warm_makes_one_explicit_stable_section_call_and_records_usage() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("OK".into()),
            StreamEvent::Usage {
                input_tokens: 2_000,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 1_800,
            },
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_leader_prices(&service, 3.0, 15.0, 0.3, 3.75);
    let session = engine
        .create_session(project.id.clone(), "warm the cache".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let before_status = engine
        .store()
        .get_session(&session.id)
        .unwrap()
        .unwrap()
        .status;
    let result = service.warm_cache_now(&session.id, None).await.unwrap();
    assert!(result.warmed);
    assert_eq!(result.role, "leader");
    assert_eq!(result.source, "manual");
    assert_eq!(result.provider, "queue");
    assert_eq!(result.cache_ttl, "5m");
    assert_eq!(result.input_tokens, 2_000);
    assert_eq!(result.cache_write_tokens, 1_800);
    assert!(result.cost_usd > 0.0);
    assert!(result.message.contains("OK"));

    {
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        let request = &recorded[0];
        // The project section is omitted when the project has no sources or
        // skills; core is always present and volatile is always absent.
        assert!((1..=2).contains(&request.system_sections.len()));
        assert_eq!(request.system_sections[0].kind, "core");
        assert!(
            !request
                .system_sections
                .iter()
                .any(|section| section.kind == "volatile")
        );
        assert!(
            !request
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("Session goal:")
        );
        assert!(request.cache.is_some());
    }

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "agent.message" || event.kind == "agent.queued"),
        "warming must not start a prompt/run"
    );
    let warm_event = events
        .iter()
        .find(|event| event.kind == "context.cache_warm")
        .expect("cache warm event");
    assert_eq!(warm_event.payload["warmed"], true);
    assert_eq!(warm_event.payload["source"], "manual");
    let cache_event = events
        .iter()
        .find(|event| event.kind == "context.cache" && event.payload["warm"] == true)
        .expect("warm cache usage event");
    assert_eq!(cache_event.payload["warm"], true);
    assert_eq!(cache_event.payload["cache_read_tokens"], 0);
    assert_eq!(cache_event.payload["cache_write_tokens"], 1_800);
    let usage = engine.usage_ledger(&session.id).await.unwrap();
    assert!(
        usage
            .entries
            .iter()
            .any(|entry| entry.role == "cache_warm" && entry.input_tokens == 2_000)
    );
    assert_eq!(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .status,
        before_status
    );

    // A disabled policy is an explicit no-op: no hidden provider call.
    let policy = vp_protocol::types::CachePolicy {
        enabled: false,
        ..Default::default()
    };
    engine.set_cache_policy(&project.id, policy).await.unwrap();
    let disabled_session = engine
        .create_session(project.id, "disabled warm".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&disabled_session.id, "test")
        .await
        .unwrap();
    let disabled = service
        .warm_cache_now(&disabled_session.id, None)
        .await
        .unwrap();
    assert!(!disabled.warmed);
    assert!(disabled.message.contains("disabled"));
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn warm_on_resume_is_opt_in_and_adds_one_stable_section_call() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("OK".into()),
                StreamEvent::Usage {
                    input_tokens: 1_000,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 800,
                },
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("resumed".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let config = service.config.read().unwrap().as_ref().unwrap().clone();
    let session = engine
        .create_session(project.id.clone(), "resume warm".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_cache_policy(
            &project.id,
            vp_protocol::types::CachePolicy {
                warm_on_resume: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let session_record = engine.store().get_session(&session.id).unwrap().unwrap();
    let project_record = engine.store().get_project(&project.id).unwrap().unwrap();
    let worktree = PathBuf::from(&project_record.root);
    let spec = AgentSpec {
        id: "leader".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: "main".into(),
        session_worktree: worktree.clone(),
        session_branch: "main".into(),
        write_scope: Vec::new(),
        base_commit: String::new(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 1,
            labour: 1,
        },
    )
    .with_unlimited_flag(engine.unlimited_flag(&session.id, false));
    let control = RunControl::new();
    let journal = crate::run_journal::RunJournal::begin(
        &engine,
        &session_record,
        &project.id,
        "leader",
        "Leader",
        "leader",
        None,
        None,
        "resume warm",
        "queue",
        "test",
        &worktree,
        RunBudgetSnapshot::default(),
        true,
    )
    .await
    .unwrap();
    let mut conversation = Vec::new();
    let mut ids = Vec::new();
    let outcome = run_loop(
        &engine,
        &config,
        &session_record,
        &project_record,
        &control,
        &budget,
        &spec,
        "resume warm",
        &mut conversation,
        Some(&mut ids),
        true,
        Some(journal),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, LoopOutcome::Final(_)));
    {
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert!(
            !recorded[0]
                .system_sections
                .iter()
                .any(|section| section.kind == "volatile"),
            "warm request must contain only stable sections"
        );
        assert!(
            recorded[1]
                .system_sections
                .iter()
                .any(|section| section.kind == "volatile"),
            "the resumed turn still carries run context"
        );
    }
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let warm = events
        .iter()
        .find(|event| event.kind == "context.cache_warm")
        .expect("resume warm event");
    assert_eq!(warm.payload["source"], "resume");
    assert_eq!(warm.payload["warmed"].as_bool(), Some(true));
    assert!(events.iter().any(|event| event.kind == "agent.message"));
}

#[tokio::test]
async fn cache_plan_previews_roles_without_a_provider_call() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_worker_model_and_prices(&service, "worker-model", 1.0, 2.0, 0.1, 1.5);
    engine
        .set_cache_policy(
            &project.id,
            vp_protocol::types::CachePolicy {
                ttl: "1h".into(),
                role_ttl: std::collections::BTreeMap::from([(
                    "worker".to_string(),
                    "5m".to_string(),
                )]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "plan preview".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = service
        .cache_plan_now(&session.id, &["leader".to_string(), "worker".to_string()])
        .await
        .unwrap();
    assert_eq!(plan.session_id, session.id);
    assert_eq!(plan.provider, "queue");
    assert_eq!(plan.roles.len(), 2);
    assert_eq!(plan.roles[0].role, "leader");
    assert_eq!(plan.roles[0].cache_ttl, "1h");
    assert_eq!(plan.roles[1].role, "worker");
    assert_eq!(plan.roles[1].model, "worker-model");
    assert_eq!(plan.roles[1].cache_ttl, "5m");
    assert_eq!(plan.roles[1].cache_breakpoints, 0);
    assert!(plan.roles[0].prefix_hash.len() == 64);
    assert!(plan.roles[0].core_hash.len() == 64);
    assert!(plan.roles[0].tool_count > 0);
    assert!(
        plan.policy
            .as_ref()
            .is_some_and(|policy| policy.ttl == "1h")
    );
    assert!(requests.lock().unwrap().is_empty());
    assert_eq!(
        service
            .cache_plan_now(&session.id, &["intern".to_string()])
            .await
            .unwrap_err()
            .to_string(),
        "unknown cache-plan role: intern"
    );
}

#[tokio::test]
async fn cache_warm_honors_a_role_model_override_and_role_ttl() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("OK".into()),
            StreamEvent::Usage {
                input_tokens: 500,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 400,
            },
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_worker_model_and_prices(&service, "worker-model", 1.0, 2.0, 0.1, 1.5);
    engine
        .set_cache_policy(
            &project.id,
            vp_protocol::types::CachePolicy {
                role_ttl: std::collections::BTreeMap::from([(
                    "worker".to_string(),
                    "1h".to_string(),
                )]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "role warm".into(), None)
        .await
        .unwrap();
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .create_agent(
            &session.id,
            "worker",
            "Worker",
            Some("Cache Worker".into()),
            Some(leader_id),
            "worker-model",
        )
        .await
        .unwrap();
    let result = service
        .warm_cache_now(&session.id, Some("worker"))
        .await
        .unwrap();
    assert!(result.warmed);
    assert_eq!(result.role, "worker");
    assert_eq!(result.model, "worker-model");
    assert_eq!(result.cache_ttl, "1h");
    assert!(result.cost_usd > 0.0);
    {
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].model, "worker-model");
        let cache = recorded[0]
            .cache
            .expect("warm request carries cache policy");
        assert_eq!(cache.ttl.as_str(), "1h");
        assert!(cache.enabled);
    }
    assert_eq!(
        service
            .warm_cache_now(&session.id, Some("intern"))
            .await
            .unwrap_err()
            .to_string(),
        "unknown cache-warm role: intern"
    );
}

#[test]
fn dynamic_mcp_tools_sort_deterministically_and_cap_after_sorting() {
    fn binding(name: &str) -> McpToolBinding {
        McpToolBinding {
            exposed_name: name.to_string(),
            server_id: "server".to_string(),
            tool_name: name.to_string(),
            pin_trust: crate::contract::McpPinTrust::Trusted,
            schema: ToolSchema {
                name: name.to_string(),
                description: String::new(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }
    }
    let names = [
        "mcp_zeta_search",
        "mcp_alpha_read",
        "mcp_middle_write",
        "mcp_beta_list",
    ];
    let sorted = sort_and_cap_dynamic_mcp_tools(names.iter().map(|name| binding(name)).collect());
    assert_eq!(
        sorted
            .iter()
            .map(|binding| binding.exposed_name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "mcp_alpha_read",
            "mcp_beta_list",
            "mcp_middle_write",
            "mcp_zeta_search"
        ]
    );

    let many = (0..40)
        .map(|index| binding(&format!("mcp_tool_{:02}", 39 - index)))
        .collect::<Vec<_>>();
    let capped = sort_and_cap_dynamic_mcp_tools(many);
    assert_eq!(capped.len(), MAX_DYNAMIC_MCP_TOOLS);
    assert_eq!(capped[0].exposed_name, "mcp_tool_00");
    assert_eq!(capped[31].exposed_name, "mcp_tool_31");
}

#[test]
fn tool_role_matrix_matches_the_tooling_roadmap() {
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "project_profile"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "git"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "lsp_query"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "read_pdf"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "read_image"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "read_document"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "archive_list"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "media_info"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "archive_extract"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "media_transcode"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Coding, "data_query"));
    assert!(AgentRole::Manager.allows_tool(AgentProfile::Research, "web_search"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "web_search"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "apply_patch"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "run_project_task"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "browser"));
    assert!(!AgentRole::Manager.allows_tool(AgentProfile::Coding, "computer"));

    assert!(AgentRole::Verifier.allows_tool(AgentProfile::Verification, "run_project_task"));
    assert!(!AgentRole::Verifier.allows_tool(AgentProfile::Verification, "apply_patch"));
    assert!(!AgentRole::Verifier.allows_tool(AgentProfile::Verification, "browser"));
    assert!(!AgentRole::Verifier.allows_tool(AgentProfile::Verification, "computer"));

    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "apply_patch"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "file_manage"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "git"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "browser"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "computer"));
    assert!(AgentRole::Labour.allows_tool(AgentProfile::Coding, "computer"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "archive_extract"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Coding, "media_transcode"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Research, "archive_extract"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Research, "media_transcode"));
    assert!(AgentRole::Scout.allows_tool(AgentProfile::Coding, "archive_list"));
    assert!(AgentRole::Scout.allows_tool(AgentProfile::Coding, "media_info"));
    assert!(!AgentRole::Scout.allows_tool(AgentProfile::Coding, "archive_extract"));
    assert!(!AgentRole::Scout.allows_tool(AgentProfile::Coding, "media_transcode"));
    assert!(AgentRole::Verifier.allows_tool(AgentProfile::Verification, "archive_list"));
    assert!(AgentRole::Verifier.allows_tool(AgentProfile::Verification, "media_info"));
    assert!(!AgentRole::Verifier.allows_tool(AgentProfile::Verification, "archive_extract"));
    assert!(!AgentRole::Worker.allows_tool(AgentProfile::Research, "browser"));
    assert!(!AgentRole::Worker.allows_tool(AgentProfile::Research, "computer"));
    assert!(!AgentRole::Worker.allows_tool(AgentProfile::Research, "apply_patch"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Research, "read_files"));
    assert!(AgentRole::Worker.allows_tool(AgentProfile::Research, "web_search"));
    assert!(!AgentRole::Worker.allows_tool(AgentProfile::Coding, "web_search"));
    assert!(!AgentRole::Worker.allows_tool(AgentProfile::Generic, "shell"));
}

#[tokio::test]
async fn executes_parallel_reads_and_structured_patch_tools() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "find".into(),
                    name: "find_files".into(),
                    arguments: r#"{"pattern":"**/*.md"}"#.into(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "patch".into(),
                    name: "apply_patch".into(),
                    arguments: serde_json::json!({
                        "patch": "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("tool round complete".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "tooling smoke".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "exercise tools").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let content = engine.workspace_file(&session.id, "new.txt").await.unwrap();
    assert_eq!(content.content, "hello\nworld\n");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let find = events
        .iter()
        .find(|event| event.kind == "tool.finished" && event.payload["name"] == "find_files")
        .expect("find_files result");
    assert!(
        find.payload["output"]
            .as_str()
            .unwrap()
            .contains("README.md")
    );
    assert!(find.payload["structured"].is_object());
    let patch = events
        .iter()
        .find(|event| event.kind == "tool.finished" && event.payload["name"] == "apply_patch")
        .expect("apply_patch result");
    assert_eq!(patch.payload["structured"]["files"][0]["status"], "created");
    assert!(!patch.payload["is_error"].as_bool().unwrap());
    assert!(
        events.iter().any(|event| {
            event.kind == "agent.turn_finished" && event.payload["parallel"] == true
        })
    );
}

#[tokio::test]
async fn stable_prompt_includes_bounded_role_filtered_recipes() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "recipes".into(), None)
        .await
        .unwrap();
    let worker = help_spec(AgentRole::Worker, AgentProfile::Coding);
    let first = role_system_prompt(
        AgentRole::Worker,
        &project,
        &session,
        &worker,
        "implement the change",
    );
    let second = role_system_prompt(
        AgentRole::Worker,
        &project,
        &session,
        &worker,
        "implement the change",
    );
    assert_eq!(first.core, second.core);
    assert_eq!(first.run_context, second.run_context);
    assert!(first.core.contains("Tool recipes"));
    assert!(first.core.contains("safe_edit"));
    assert!(!first.core.contains("read_tool_output (retryable)"));
    assert!(first.core.contains("runtime commits your worktree"));
    assert!(first.core.contains("return immediately"));
    assert!(!first.core.contains("Inspect first"));
    assert!(
        first.core.chars().count() < 12_000,
        "stable prefix unexpectedly large: {}",
        first.core.chars().count()
    );

    let verifier = help_spec(AgentRole::Verifier, AgentProfile::Verification);
    let verifier_prompt = role_system_prompt(
        AgentRole::Verifier,
        &project,
        &session,
        &verifier,
        "verify the change",
    );
    assert!(verifier_prompt.core.contains("test_and_check"));
    assert!(!verifier_prompt.core.contains("safe_edit:"));
    assert!(!verifier_prompt.core.contains("gui_verification:"));

    let leader = help_spec(AgentRole::Leader, AgentProfile::Generic);
    let leader_prompt =
        role_system_prompt(AgentRole::Leader, &project, &session, &leader, "coordinate");
    assert!(leader_prompt.core.contains("remote_operation"));
    assert!(leader_prompt.core.contains("gui_verification"));
}

#[test]
fn tool_help_is_read_only_role_filtered_and_bounded() {
    let config_dir = TempDir::new().unwrap();
    let leader = help_spec(AgentRole::Leader, AgentProfile::Generic);
    let output = run_tool_help(&leader, &json!({}));
    assert!(!output.is_error);
    assert!(output.content.contains("read_many_files"));
    assert!(output.content.contains("safe_edit"));
    assert!(output.content.chars().count() <= crate::tooling::recipes::MAX_HELP_RENDER_CHARS);
    let structured = output.structured.unwrap();
    assert_eq!(structured["executed_tool"], false);
    assert_eq!(structured["role"], "leader");
    assert_eq!(
        structured["catalog_hash"],
        crate::tooling::recipes::catalog_hash()
    );
    assert!(structured["recipes"].as_array().unwrap().len() >= 8);

    let verifier = help_spec(AgentRole::Verifier, AgentProfile::Verification);
    let output = run_tool_help(&verifier, &json!({"recipe": "safe_edit"}));
    assert!(output.is_error);
    assert_eq!(output.structured.unwrap()["code"], "INVALID_ARGUMENT");
    assert!(output.content.contains("No recipe 'safe_edit' applies"));

    let worker = help_spec(AgentRole::Worker, AgentProfile::Coding);
    let output = run_tool_help(&worker, &json!({"recipe": "safe_edit", "max_chars": 1000}));
    assert!(!output.is_error);
    assert!(output.content.contains("expected_sha256"));
    assert!(output.content.chars().count() <= 1000);

    // Repeated calls are byte-identical for the same role/profile.
    let first = run_tool_help(&worker, &json!({"recipe": "test_and_check"}));
    let second = run_tool_help(&worker, &json!({"recipe": "test_and_check"}));
    assert_eq!(first.content, second.content);
    assert_eq!(first.structured, second.structured);

    let code = run_tool_help(&worker, &json!({"error_code": "PATCH_CONFLICT"}));
    assert!(!code.is_error);
    assert!(code.content.contains("PATCH_CONFLICT"));
    assert!(code.content.contains("retryable: yes"));
    let structured = code.structured.unwrap();
    assert_eq!(structured["error_code"]["code"], "PATCH_CONFLICT");
    assert_eq!(structured["error_code"]["retryable"], true);

    let unknown = run_tool_help(&worker, &json!({"error_code": "NOT_A_CODE"}));
    assert!(unknown.is_error);
    assert_eq!(unknown.structured.unwrap()["code"], "INVALID_ARGUMENT");
    assert!(unknown.content.contains("include_error_codes"));

    let catalog = run_tool_help(
        &worker,
        &json!({"include_error_codes": true, "max_chars": 8000}),
    );
    assert!(!catalog.is_error);
    assert!(catalog.content.contains("[file]"));
    assert!(catalog.content.contains("PATCH_CONFLICT"));
    assert!(catalog.content.chars().count() <= 8000);

    // The catalog is guidance, not a schema mutation: the advertised bundle is
    // byte-identical before and after a help call for every role.
    let schemas_before = tool_schemas(
        &leader_config(&config_dir),
        AgentRole::Worker,
        AgentProfile::Coding,
        false,
    );
    let _ = run_tool_help(&worker, &json!({"recipe": "safe_edit"}));
    let schemas_after = tool_schemas(
        &leader_config(&config_dir),
        AgentRole::Worker,
        AgentProfile::Coding,
        false,
    );
    assert_eq!(
        serde_json::to_value(&schemas_before).unwrap(),
        serde_json::to_value(&schemas_after).unwrap()
    );
    let help = schemas_before
        .iter()
        .find(|schema| schema.name == "tool_help")
        .expect("tool_help schema");
    assert!(!help.parameters["properties"]["error_code"].is_null());
    assert!(!help.parameters["properties"]["include_error_codes"].is_null());
    assert!(!help.parameters["properties"]["recipe"].is_null());
}

fn leader_config(dir: &TempDir) -> AgentConfig {
    AgentConfig {
        provider: Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::new()),
        }),
        provider_name: "queue".into(),
        model: "test".into(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 1,
        max_tokens: Some(128),
        max_managers: 1,
        max_workers: 1,
        max_labour: 1,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 1,
        scope_locks: Arc::new(ScopeLocks::with_store(Store::open(dir.path()).unwrap())),
        capacity: Arc::new(CapacityController::new(1, 1)),
        provider_idle_timeout_secs: 10,
        question_timeout_secs: 10,
        approval_mode: ApprovalMode::default(),
        approval_timeout_secs: 10,
        compact_after_tokens: 0,
        compact_keep_messages: 4,
        compact_keep_tokens: 1_000,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    }
}

#[tokio::test]
async fn tool_finished_events_include_metrics_and_failure_contract() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "missing".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"does-not-exist.txt"}"#.into(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "ok".into(),
                    name: "find_files".into(),
                    arguments: r#"{"pattern":"*.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "tool metrics".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "check metrics").await.unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let missing = events
        .iter()
        .find(|event| event.kind == "tool.finished" && event.payload["tool_call_id"] == "missing")
        .expect("missing read result");
    assert_eq!(missing.payload["is_error"], true);
    assert_eq!(missing.payload["error_code"], "FILE_NOT_FOUND");
    assert_eq!(missing.payload["retryable"], false);
    assert_eq!(missing.payload["approval_mode"], "smart");
    assert!(missing.payload["duration_ms"].as_u64().is_some());
    assert!(missing.payload["output_bytes"].as_u64().is_some());
    assert!(missing.payload["approval_risk"].as_str().is_some());
    assert!(
        missing.payload["error_hint"]
            .as_str()
            .unwrap()
            .contains("find_files")
    );
    assert!(
        missing.payload["output"]
            .as_str()
            .unwrap()
            .starts_with("error[FILE_NOT_FOUND]")
    );

    let ok = events
        .iter()
        .find(|event| event.kind == "tool.finished" && event.payload["tool_call_id"] == "ok")
        .expect("find result");
    assert_eq!(ok.payload["is_error"], false);
    assert!(ok.payload["error_code"].is_null());
    assert_eq!(ok.payload["retryable"], false);
    assert!(ok.payload["duration_ms"].as_u64().is_some());
    assert!(ok.payload["output_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn parallel_tool_events_carry_independent_metrics() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "find".into(),
                    name: "find_files".into(),
                    arguments: r#"{"pattern":"*.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("parallel done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "parallel metrics".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "read in parallel")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.turn_finished" && event.payload["parallel"] == true)
    );
    for call_id in ["read", "find"] {
        let event = events
            .iter()
            .find(|event| event.kind == "tool.finished" && event.payload["tool_call_id"] == call_id)
            .unwrap_or_else(|| panic!("missing tool.finished for {call_id}"));
        assert_eq!(event.payload["is_error"], false);
        assert!(event.payload["duration_ms"].as_u64().is_some());
        assert!(event.payload["output_bytes"].as_u64().unwrap() > 0);
        assert_eq!(event.payload["approval_mode"], "smart");
        assert_eq!(event.payload["approval_risk"], "read_only");
        assert!(event.payload["error_code"].is_null());
        assert_eq!(event.payload["retryable"], false);
    }
}

#[tokio::test]
async fn session_tool_activity_is_bounded_and_projects_legacy_events() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let project_dir = dir.path().join("repo");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("README.md"), "activity\n").unwrap();
    init_repo(&project_dir);
    let project = engine
        .store()
        .add_project(project_dir.to_str().unwrap(), Some("repo"))
        .unwrap();
    let session = engine
        .create_session(project.id, "tool activity".into(), None)
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "tool.finished",
            json!({
                "tool_call_id": "old",
                "name": "read_file",
                "output": "failed",
                "is_error": true,
                "structured": {"code": "FILE_NOT_FOUND"}
            }),
        )
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "tool.finished",
            json!({
                "tool_call_id": "new",
                "name": "find_files",
                "output": "ok",
                "is_error": false,
                "duration_ms": 42,
                "output_bytes": 120,
                "truncated": true,
                "approval_risk": "read_only",
                "approval_mode": "strict",
                "approval_wait_ms": 25
            }),
        )
        .await
        .unwrap();
    let activity = engine.session_tool_activity(&session.id, 1).await.unwrap();
    assert_eq!(activity.limit, 1);
    assert_eq!(activity.total, 2);
    assert!(activity.truncated);
    assert_eq!(activity.entries.len(), 1);
    assert_eq!(activity.entries[0].tool_call_id, "new");
    assert_eq!(activity.entries[0].duration_ms, 42);
    assert_eq!(activity.entries[0].output_bytes, 120);
    assert!(activity.entries[0].truncated);
    assert_eq!(activity.entries[0].approval_mode.as_deref(), Some("strict"));
    assert_eq!(activity.summary.calls, 1);
    assert_eq!(activity.summary.max_duration_ms, 42);
    assert_eq!(activity.summary.approval_waits, 1);
    assert_eq!(activity.summary.max_approval_wait_ms, 25);
    assert!(activity.summary.error_codes.is_empty());

    let activity = engine.session_tool_activity(&session.id, 50).await.unwrap();
    assert!(!activity.truncated);
    let legacy = activity
        .entries
        .iter()
        .find(|entry| entry.tool_call_id == "old")
        .expect("legacy entry");
    assert!(legacy.is_error);
    assert_eq!(legacy.error_code.as_deref(), Some("FILE_NOT_FOUND"));
    assert!(!legacy.retryable);
    assert_eq!(legacy.duration_ms, 0);
    assert!(legacy.hint.as_deref().unwrap().contains("find_files"));
    assert_eq!(activity.summary.errors, 1);
    assert_eq!(activity.summary.error_codes[0].code, "FILE_NOT_FOUND");
}

#[tokio::test]
async fn tool_activity_projection_caps_names_hints_and_counts() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let project_dir = dir.path().join("repo");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("README.md"), "activity\n").unwrap();
    init_repo(&project_dir);
    let project = engine
        .store()
        .add_project(project_dir.to_str().unwrap(), Some("repo"))
        .unwrap();
    let session = engine
        .create_session(project.id, "tool bounds".into(), None)
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "tool.finished",
            json!({
                "tool_call_id": "x".repeat(500),
                "name": "n".repeat(500),
                "is_error": true,
                "error_code": "UNKNOWN_LEGACY_CODE",
                "error_hint": "h".repeat(10_000),
                "duration_ms": u64::MAX,
                "output_bytes": 7,
                "approval_wait_ms": u64::MAX,
            }),
        )
        .await
        .unwrap();
    let activity = engine.session_tool_activity(&session.id, 50).await.unwrap();
    let entry = &activity.entries[0];
    assert_eq!(entry.tool_call_id.chars().count(), 200);
    assert_eq!(entry.name.chars().count(), 120);
    assert_eq!(entry.error_code.as_deref(), Some("TOOL_FAILED"));
    assert_eq!(entry.hint.as_ref().unwrap().chars().count(), 600);
    assert_eq!(entry.duration_ms, 86_400_000);
    assert_eq!(entry.approval_wait_ms, Some(86_400_000));
    assert_eq!(activity.summary.error_codes[0].code, "TOOL_FAILED");
}

#[tokio::test]
async fn exposes_runtime_budget_search_and_artifact_tools() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "budget".into(),
                    name: "budget_status".into(),
                    arguments: "{}".into(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "search".into(),
                    name: "tool_search".into(),
                    arguments: r#"{"query":"read file"}"#.into(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "search_browser".into(),
                    name: "tool_search".into(),
                    arguments: r#"{"query":"browser"}"#.into(),
                }),
                StreamEvent::ToolCall(ToolCall {
                    id: "artifact".into(),
                    name: "publish_artifact_file".into(),
                    arguments: r#"{"path":"README.md","name":"readme","kind":"report"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("runtime tools complete".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "runtime tools".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "inspect runtime tools")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let budget = events
        .iter()
        .find(|event| event.kind == "tool.finished" && event.payload["name"] == "budget_status")
        .expect("budget result");
    assert!(budget.payload["structured"]["session"].is_object());
    assert_eq!(budget.payload["structured"]["unlimited"], false);
    let search = events
        .iter()
        .find(|event| event.kind == "tool.finished" && event.payload["name"] == "tool_search")
        .expect("tool search result");
    assert!(
        search.payload["output"]
            .as_str()
            .unwrap()
            .contains("read_file")
    );
    assert_eq!(search.payload["structured"]["bundle"], "leader");
    let read_file = search.payload["structured"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "read_file")
        .expect("read_file search result");
    assert_eq!(read_file["in_bundle"], true);
    assert_eq!(read_file["category"], "files");
    let browser_search = events
        .iter()
        .find(|event| {
            event.kind == "tool.finished"
                && event.payload["name"] == "tool_search"
                && event.payload["tool_call_id"] == "search_browser"
        })
        .expect("browser tool search result");
    assert!(
        browser_search.payload["output"]
            .as_str()
            .unwrap()
            .contains("browser")
    );
    let browser = browser_search.payload["structured"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "browser")
        .expect("browser search result");
    assert_eq!(
        browser["in_bundle"], false,
        "a generic Leader bundle does not advertise GUI tools"
    );
    assert_eq!(browser_search.payload["structured"]["bundle"], "leader");
    assert!(
        browser_search.payload["structured"]["outside_bundle"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    let artifact = events
        .iter()
        .find(|event| {
            event.kind == "tool.finished" && event.payload["name"] == "publish_artifact_file"
        })
        .expect("artifact result");
    assert!(!artifact.payload["is_error"].as_bool().unwrap());
    let artifacts = engine.list_artifacts(&session.id).await.unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].name, "readme");
    assert_eq!(artifacts[0].kind, "report");
}

#[tokio::test]
async fn task_detail_and_control_wrappers_are_event_backed() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, _service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "task tools".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "task tools".into(),
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
            write_scope: vec!["src/t1".into()],
            acceptance: vec!["done".into()],
            token_budget: Some(1000),
            cost_budget_usd: None,
        }],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let spec = AgentSpec {
        id: "agent-1".into(),
        role: AgentRole::Leader,
        name: "Leader".into(),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: Some("T1".into()),
        department: Some("impl".into()),
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: PathBuf::from(session.worktree.clone().unwrap()),
        branch: session.branch.clone().unwrap(),
        session_worktree: PathBuf::from(session.worktree.clone().unwrap()),
        session_branch: session.branch.clone().unwrap(),
        write_scope: vec!["src/t1".into()],
        base_commit: session.base_commit.clone().unwrap_or_default(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let detail = run_task_detail(&engine, &session, &spec, serde_json::json!({}))
        .await
        .unwrap();
    assert!(!detail.is_error, "{}", detail.content);
    assert_eq!(detail.structured.as_ref().unwrap()["task_id"], "T1");
    assert_eq!(detail.structured.as_ref().unwrap()["acceptance"][0], "done");
    let skipped = run_task_control(
        &engine,
        &session,
        &spec,
        serde_json::json!({"action": "skip", "task_id": "T1"}),
    )
    .await
    .unwrap();
    assert!(!skipped.is_error, "{}", skipped.content);
    assert_eq!(
        skipped.structured.as_ref().unwrap()["task"]["status"],
        "skipped"
    );
}

fn continuation_test_plan() -> TeamPlan {
    TeamPlan {
        goal: "deferred continuation".into(),
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
            title: "Foundation".into(),
            kind: "coding".into(),
            priority: 0,
            profile: None,
            deps: Vec::new(),
            write_scope: vec!["src/one".into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    }
}

#[tokio::test]
async fn resume_plan_fails_closed_on_incompatible_runtime_snapshot() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "pin".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = continuation_test_plan();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let mut rewritten = plan;
    rewritten.tasks[0].write_scope = vec!["src/other".into()];
    engine
        .store()
        .set_team_plan(&session.id, &serde_json::to_string(&rewritten).unwrap())
        .unwrap();
    let error = service.resume_plan(&session.id).await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(rendered.contains("RUNTIME_INCOMPATIBLE"), "{rendered}");
    let events = engine.all_events(&session.id).await.unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "agent.plan_resume_queued")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == crate::runtime_pin::PLAN_MIGRATED_EVENT)
    );
}

#[tokio::test]
async fn resume_plan_records_visible_drift_before_other_checks() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "drift".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    let mut policy = engine
        .orchestration_policy(&session.project_id)
        .await
        .unwrap();
    policy.max_attempts = policy.max_attempts.saturating_add(1);
    engine
        .set_orchestration_policy(&session.project_id, &policy)
        .await
        .unwrap();
    let error = service.resume_plan(&session.id).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("already complete"),
        "{error:#}"
    );
    let events = engine.all_events(&session.id).await.unwrap();
    let drift = events
        .iter()
        .find(|event| event.kind == crate::runtime_pin::COMPATIBILITY_DRIFT_EVENT)
        .expect("drift event");
    assert_eq!(drift.payload["status"], "drift");
    assert_eq!(drift.payload["target"], "plan");
}

#[tokio::test]
async fn deferred_continuation_waits_for_active_session_and_delivers_once() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "defer".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "summarize the foundation work",
            vec!["T1".into()],
            Some("all"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine.skip_plan_task(&session.id, "T1").await.unwrap();

    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let readied = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(readied.continuations[0].status, "ready");
    assert_eq!(
        readied.continuations[0].wake_reason.as_deref(),
        Some("watch")
    );
    assert_eq!(readied.continuations[0].outcomes[0].status, "skipped");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.ready")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );

    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 1);
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let delivered = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(delivered.continuations[0].id, continuation.id);
    assert_eq!(delivered.continuations[0].status, "delivered");
    assert!(delivered.continuations[0].delivered_at.is_some());

    let messages = engine.list_messages(&session.id).await.unwrap();
    let prompt = messages
        .iter()
        .find(|message| {
            message.role == "user"
                && message
                    .content
                    .starts_with(&format!("[Deferred continuation #{}]", continuation.id))
        })
        .expect("deferred prompt was delivered as a user message");
    assert!(prompt.content.contains("Deferred reason: watch"));
    assert!(prompt.content.contains("- T1 \"Foundation\": skipped"));
    assert!(prompt.content.contains("summarize the foundation work"));

    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivering")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
    assert_eq!(
        engine
            .list_messages(&session.id)
            .await
            .unwrap()
            .iter()
            .filter(|message| message.content.starts_with("[Deferred continuation #"))
            .count(),
        1
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn recurring_continuation_delivers_first_run_and_leaves_child_pending() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "recurring deliver".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "report the foundation",
            vec!["T1".into()],
            Some("all"),
            None,
            None,
            None,
            Some(60),
            Some(3),
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 1);
    let _ = wait_until_idle(&engine, &session).await;

    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 2);
    let delivered = rows.iter().find(|row| row.id == continuation.id).unwrap();
    assert_eq!(delivered.status, "delivered");
    assert_eq!(delivered.run_count, 1);
    let child = rows
        .iter()
        .find(|row| row.parent_continuation_id.as_deref() == Some(continuation.id.as_str()))
        .expect("recurring child exists");
    assert_eq!(child.status, "pending");
    assert_eq!(child.run_count, 1);
    assert_eq!(child.series_id, continuation.series_id);
    assert!(child.watch_task_ids.is_empty());
    assert_eq!(child.watch_mode, "all");
    assert!(child.deadline_at.is_some());

    let messages = engine.list_messages(&session.id).await.unwrap();
    let prompt = messages
        .iter()
        .find(|message| {
            message.role == "user"
                && message
                    .content
                    .starts_with(&format!("[Deferred continuation #{}]", continuation.id))
        })
        .expect("deferred prompt was delivered");
    assert!(prompt.content.contains(&format!(
        "Recurring series: {} · run 1/3 · every 60s",
        continuation.series_id
    )));

    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 2, "the child must not create another row");
    assert_eq!(
        rows.iter().find(|row| row.id == child.id).unwrap().status,
        "pending",
        "the child waits for its interval even though T1 is terminal"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.scheduled")
            .count(),
        1
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn recurring_continuation_start_failure_does_not_schedule_child() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("recurring failure"))
        .unwrap();
    let session = engine
        .create_session(project.id, "recurring failure".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "never starts",
            vec!["T1".into()],
            None,
            None,
            None,
            Some(1),
            Some(60),
            Some(3),
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let service = AgentService::unconfigured(engine.clone());
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, continuation.id);
    assert_eq!(rows[0].status, "failed");
    assert_eq!(rows[0].run_count, 0);
    assert_eq!(rows[0].attempts, 1);
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.scheduled")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn deferred_continuation_start_failures_back_off_and_respect_attempt_cap() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("continuation failure"))
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "defer failure".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();

    let failed = engine
        .defer_session(
            &session.id,
            "one attempt",
            vec!["T1".into()],
            None,
            None,
            None,
            Some(1),
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    let retrying = engine
        .defer_session(
            &session.id,
            "three attempts",
            vec!["T1".into()],
            None,
            None,
            None,
            Some(3),
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let service = AgentService::unconfigured(engine.clone());
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let listed = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    let row = listed
        .continuations
        .iter()
        .find(|continuation| continuation.id == failed.id)
        .unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.attempts, 1);
    assert!(
        row.last_error
            .as_deref()
            .is_some_and(|error| error.contains("provider is not configured"))
    );
    let row = listed
        .continuations
        .iter()
        .find(|continuation| continuation.id == retrying.id)
        .unwrap();
    assert_eq!(row.status, "ready");
    assert_eq!(row.attempts, 1);
    assert!(row.next_attempt_at.is_some());
    assert!(row.last_error.is_some());
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.failed")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.retrying")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.delivering")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn deferred_continuations_cancel_for_landed_and_imported_sessions() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("continuation finality"))
        .unwrap();
    let service = AgentService::unconfigured(engine.clone());

    for final_status in ["landed", "imported"] {
        let session = engine
            .create_session(project.id.clone(), format!("defer {final_status}"), None)
            .await
            .unwrap();
        engine
            .set_team_plan(&session.id, &continuation_test_plan())
            .await
            .unwrap();
        let continuation = engine
            .defer_session(
                &session.id,
                "too late",
                vec!["T1".into()],
                None,
                None,
                None,
                None,
                Some(60),
                Some(3),
                None,
            )
            .await
            .unwrap()
            .continuation;
        engine.skip_plan_task(&session.id, "T1").await.unwrap();
        engine
            .update_session_status(&session.id, final_status)
            .await
            .unwrap();
        assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
        let listed = engine
            .session_continuations(&session.id, None, Some(10))
            .await
            .unwrap();
        assert_eq!(listed.continuations.len(), 1);
        assert_eq!(listed.continuations[0].id, continuation.id);
        assert_eq!(listed.continuations[0].status, "cancelled");
        assert!(listed.continuations[0].last_error.is_some());
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "session.continuation.cancelled")
        );
        assert!(
            !events
                .iter()
                .any(|event| event.kind == "session.continuation.scheduled")
        );
    }
}

#[tokio::test]
async fn deferred_continuation_deadline_fallback_wakes_without_watched_tasks() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("continuation deadline"))
        .unwrap();
    let session = engine
        .create_session(project.id, "defer by deadline".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "deadline follow-up",
            Vec::new(),
            None,
            None,
            Some(1),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    let service = AgentService::unconfigured(engine.clone());
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let before = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(before.continuations[0].status, "pending");
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    let after = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(after.continuations[0].id, continuation.id);
    assert_eq!(after.continuations[0].status, "ready");
    assert_eq!(
        after.continuations[0].wake_reason.as_deref(),
        Some("deadline")
    );
    assert!(after.continuations[0].outcomes.is_empty());
    assert_eq!(after.continuations[0].attempts, 1);
    assert!(after.continuations[0].last_error.is_some());
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn deferred_continuation_claim_has_one_winner_and_recovery_fails_closed() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store);
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("continuation claim"))
        .unwrap();
    let session = engine
        .create_session(project.id, "claim".into(), None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "claim once",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .mark_session_continuation_ready(&continuation.id, "watch", &[])
        .await
        .unwrap()
        .unwrap();

    let (left, right) = tokio::join!(
        engine.claim_session_continuation(&continuation.id),
        engine.claim_session_continuation(&continuation.id)
    );
    let winners = usize::from(left.unwrap().is_some()) + usize::from(right.unwrap().is_some());
    assert_eq!(winners, 1, "exactly one reconciliation pass may claim");
    let listed = engine
        .session_continuations(&session.id, Some("delivering"), Some(10))
        .await
        .unwrap();
    assert_eq!(listed.continuations.len(), 1);
    assert_eq!(listed.continuations[0].status, "delivering");

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivering")
            .count(),
        1
    );

    let service = AgentService::unconfigured(engine.clone());
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);
    assert!(
        !engine
            .list_messages(&session.id)
            .await
            .unwrap()
            .iter()
            .any(|message| message.content.starts_with("[Deferred continuation #"))
    );
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );

    engine.recover_interrupted_sessions().await.unwrap();
    let recovered = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(recovered.continuations[0].id, continuation.id);
    // A live claim is left alone. Recovery only reclaims once the claim TTL
    // has elapsed; it never fails a claim that a live process may still own.
    assert_eq!(recovered.continuations[0].status, "delivering");
    assert!(recovered.continuations[0].lease_owner.is_some());
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 0);

    // Deterministically expire the claim, then reclaim it. The old owner can
    // no longer acknowledge because its token belongs to the previous
    // generation.
    let expired_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    let reclaimed = engine
        .store()
        .reclaim_expired_session_continuation_at(&continuation.id, "test expiry", expired_at)
        .unwrap()
        .expect("expired claim was reclaimable");
    assert_eq!(reclaimed.0.status, "ready");
    assert!(reclaimed.0.lease_owner.is_none());
    assert!(
        engine
            .mark_session_continuation_delivered(&continuation.id)
            .await
            .unwrap()
            .is_none(),
        "a stale delivery token must not write a terminal state"
    );
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows[0].status, "ready");
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.continuation.reclaimed" && event.payload["reason"] == "test expiry"
    }));
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

fn stall_recovery_plan() -> TeamPlan {
    TeamPlan {
        goal: "recover a stalled task".into(),
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
            title: "Stalled task".into(),
            kind: "coding".into(),
            priority: 10,
            profile: None,
            deps: Vec::new(),
            write_scope: vec!["src/t1".into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    }
}

async fn stage_stalled_task(
    engine: &Engine,
    session: &Session,
    mode: vp_protocol::types::StallRecoveryMode,
) -> (u64, String) {
    engine
        .set_team_plan(&session.id, &stall_recovery_plan())
        .await
        .unwrap();
    let policy = vp_protocol::types::OrchestrationPolicy {
        stall_recovery: mode,
        stall_min_observations: 1,
        stall_max_attempts: 1,
        stall_grace_secs: 300,
        ..Default::default()
    };
    engine
        .set_orchestration_policy(&session.project_id, &policy)
        .await
        .unwrap();
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap();
    let task = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T1")
        .unwrap()
        .clone();
    let (claimed, _events) = engine
        .scheduler()
        .claim(
            &session.id,
            task.plan_seq,
            std::slice::from_ref(&task.task_id),
        )
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), now)
            .await
            .unwrap(),
        1
    );
    (task.plan_seq, task.task_id)
}

#[tokio::test]
async fn stall_recovery_waits_for_idle_then_retries_and_resumes_once() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let provider_trait: Arc<dyn Provider> = provider;
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "recover".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let (plan_seq, task_id) = stage_stalled_task(
        &engine,
        &session,
        vp_protocol::types::StallRecoveryMode::Retry,
    )
    .await;
    engine
        .store()
        .finish_plan_task(
            &session.id,
            plan_seq,
            &task_id,
            "cancelled",
            Some("cancelled"),
        )
        .unwrap();
    engine
        .append_event(
            &session.id,
            "plan_task.finished",
            json!({"task_id": task_id, "task_status": "cancelled"}),
        )
        .await
        .unwrap();

    assert_eq!(service.reconcile_stall_recoveries().await.unwrap(), 0);
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert!(events.iter().all(|event| event.kind != "plan_task.retried"));

    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    assert_eq!(service.reconcile_stall_recoveries().await.unwrap(), 1);
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "plan_task.retried")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "orchestration.recovery_retried")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.plan_resume_queued")
            .count(),
        1
    );
    let stalled = service.reconcile_stall_recoveries().await.unwrap();
    assert_eq!(stalled, 0);

    let settled = wait_until_idle(&engine, &session).await;
    assert_eq!(settled.status, "idle");
    let recoveries = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recoveries.len(), 1);
    assert_eq!(recoveries[0].status, "retried");
    assert_eq!(recoveries[0].attempts, 1);
}

#[tokio::test]
async fn stall_recovery_completed_before_cancel_is_unnecessary() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let provider_trait: Arc<dyn Provider> = provider;
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "recover".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let (plan_seq, task_id) = stage_stalled_task(
        &engine,
        &session,
        vp_protocol::types::StallRecoveryMode::Retry,
    )
    .await;
    engine
        .store()
        .finish_plan_task(&session.id, plan_seq, &task_id, "completed", None)
        .unwrap();
    engine
        .append_event(
            &session.id,
            "plan_task.finished",
            json!({"task_id": task_id, "task_status": "completed"}),
        )
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();

    assert_eq!(service.reconcile_stall_recoveries().await.unwrap(), 1);
    let recoveries = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recoveries[0].status, "unnecessary");
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert!(events.iter().all(|event| event.kind != "plan_task.retried"));
    assert!(
        events
            .iter()
            .all(|event| event.kind != "agent.plan_resume_queued")
    );
}

#[tokio::test]
async fn stall_recovery_budget_exhaustion_does_not_resume() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let provider_trait: Arc<dyn Provider> = provider;
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "recover".into(), Some(10))
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let (plan_seq, task_id) = stage_stalled_task(
        &engine,
        &session,
        vp_protocol::types::StallRecoveryMode::Retry,
    )
    .await;
    engine
        .store()
        .finish_plan_task(
            &session.id,
            plan_seq,
            &task_id,
            "cancelled",
            Some("cancelled"),
        )
        .unwrap();
    engine
        .append_event(
            &session.id,
            "plan_task.finished",
            json!({"task_id": task_id, "task_status": "cancelled"}),
        )
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &session.id,
            "worker-1",
            "Worker",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test",
            20,
            0,
            false,
            0.0,
        )
        .await
        .unwrap();

    assert_eq!(service.reconcile_stall_recoveries().await.unwrap(), 1);
    let recoveries = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recoveries[0].status, "failed");
    assert!(
        recoveries[0]
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("budget"))
    );
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert!(events.iter().all(|event| event.kind != "plan_task.retried"));
    assert!(
        events
            .iter()
            .all(|event| event.kind != "agent.plan_resume_queued")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "orchestration.recovery_failed")
    );
    assert!(
        engine
            .coordination_notes(&session.id, 100)
            .await
            .unwrap()
            .iter()
            .any(|note| note.action == "escalate")
    );
}

#[tokio::test]
async fn task_result_memory_carries_evidence_and_retries_supersede() {
    let data = tempfile::TempDir::new().unwrap();
    let repo = tempfile::TempDir::new().unwrap();
    std::fs::write(repo.path().join("README.md"), "memory\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@verlinpark.local"],
        vec!["config", "user.name", "VerlinPark Test"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "initial"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let base = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    std::fs::write(repo.path().join("README.md"), "memory v2\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-q", "-m", "task change"]] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let commit = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();

    let engine = Engine::new(vp_store::Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory evidence".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "evidence".into(), None)
        .await
        .unwrap();
    let (entry, _) = engine
        .store()
        .enqueue_merge_with_event(
            &session.id,
            "worker-1",
            "Worker",
            Some("T1"),
            "vp/task",
            &commit,
            &base,
            None,
        )
        .unwrap();
    engine
        .store()
        .update_merge_queue_entry_with_event(&entry.id, "merged", Some("ok"))
        .unwrap();

    super::record_task_memory(&engine, &session.id, "T1", "First task summary.").await;
    let active = engine
        .store()
        .list_memories_filtered(
            &project.id,
            None,
            Some("task_result"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let memory = &active[0];
    assert_eq!(memory.source, "task_result");
    assert!(memory.verify_after.is_some());
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "task" && evidence.reference == "T1" })
    );
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "session" && evidence.reference == session.id })
    );
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "commit" && evidence.reference == commit })
    );
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "path" && evidence.reference == "README.md")
    );

    super::record_task_memory(&engine, &session.id, "T1", "Second task summary.").await;
    let active = engine
        .store()
        .list_memories_filtered(
            &project.id,
            None,
            Some("task_result"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    assert!(active[0].body.contains("Second task summary"));
    assert_eq!(active[0].supersedes.len(), 1);
    let superseded = engine
        .store()
        .list_memories_filtered(
            &project.id,
            None,
            Some("task_result"),
            Some("superseded"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(superseded.len(), 1);
    assert!(superseded[0].body.contains("First task summary"));
    assert_eq!(
        superseded[0].superseded_by.as_deref(),
        Some(active[0].id.as_str())
    );
}

async fn memory_capture_fixture() -> (TempDir, TempDir, Engine, Session) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    std::fs::write(repo.path().join("README.md"), "memory\n").unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("capture".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "verification work".into(), None)
        .await
        .unwrap();
    (data, repo, engine, session)
}

fn capture_verdict(approved: bool, summary: &str) -> super::VerifierVerdict {
    super::VerifierVerdict {
        approved,
        summary: summary.to_string(),
    }
}

#[tokio::test]
async fn verification_memory_captures_approved_verdict_with_evidence() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    let policy = vp_protocol::types::MemoryPolicy {
        default_verify_days: Some(7),
        ..vp_protocol::types::MemoryPolicy::default()
    };
    engine
        .set_memory_policy(&session.project_id, policy)
        .await
        .unwrap();
    super::record_verification_memory(
        &engine,
        &session.id,
        Some("T1"),
        "Worker 1",
        "abcdef1234567890",
        &capture_verdict(true, "Tests pass and the diff matches the task."),
        Some("merged; integration checks passed"),
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let memory = &active[0];
    assert_eq!(memory.title, "Task T1 verification");
    assert_eq!(memory.kind, "verification");
    assert_eq!(memory.source, "verifier");
    assert!(memory.tags.contains(&"T1".to_string()));
    assert!(memory.tags.contains(&"verification".to_string()));
    assert!(memory.body.contains("approved: true"));
    assert!(memory.body.contains("verdict: Tests pass"));
    assert!(memory.body.contains("integration: merged;"));
    assert!(memory.body.contains("commit: abcdef1234567890"));
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "task" && evidence.reference == "T1")
    );
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "session" && evidence.reference == session.id })
    );
    assert!(
        memory.evidence.iter().any(|evidence| {
            evidence.kind == "commit" && evidence.reference == "abcdef1234567890"
        })
    );
    let verify_after = memory.verify_after.as_deref().expect("verify_after");
    let when =
        time::OffsetDateTime::parse(verify_after, &time::format_description::well_known::Rfc3339)
            .unwrap();
    let days = (when - time::OffsetDateTime::now_utc()).whole_days();
    assert!((6..=7).contains(&days), "unexpected TTL: {days} days");
}

#[tokio::test]
async fn verification_memory_never_claims_success_for_a_rejection() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    super::record_verification_memory(
        &engine,
        &session.id,
        Some("T2"),
        "Worker 2",
        "deadbeefdeadbeef",
        &capture_verdict(false, "Acceptance criteria are not met."),
        None,
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let body = &active[0].body;
    assert!(body.contains("approved: false"));
    assert!(!body.contains("approved: true"));
    assert!(!body.contains("merged"));
    assert!(!body.contains("integration:"));
    assert!(body.contains("verdict: Acceptance criteria are not met."));
    assert!(body.contains("commit: deadbeefdeadbeef"));
}

#[tokio::test]
async fn changed_verification_verdict_supersedes_the_previous_memory() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    super::record_verification_memory(
        &engine,
        &session.id,
        Some("T3"),
        "Worker 3",
        "1111111111111111",
        &capture_verdict(true, "Initial verdict."),
        Some("merged; ok"),
    )
    .await;
    super::record_verification_memory(
        &engine,
        &session.id,
        Some("T3"),
        "Worker 3",
        "2222222222222222",
        &capture_verdict(false, "Re-verification found a regression."),
        None,
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    assert!(
        active[0]
            .body
            .contains("Re-verification found a regression.")
    );
    assert_eq!(active[0].supersedes.len(), 1);
    let superseded = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("superseded"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(superseded.len(), 1);
    assert!(superseded[0].body.contains("Initial verdict."));
    assert_eq!(
        superseded[0].superseded_by.as_deref(),
        Some(active[0].id.as_str())
    );
}

#[tokio::test]
async fn identical_verification_capture_is_idempotent() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    let verdict = capture_verdict(true, "Same verdict.");
    for _ in 0..2 {
        super::record_verification_memory(
            &engine,
            &session.id,
            Some("T4"),
            "Worker 4",
            "3333333333333333",
            &verdict,
            Some("merged; stable"),
        )
        .await;
    }
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let events = engine
        .list_events(session.id.clone(), 0, 1_000)
        .await
        .unwrap();
    let created = events
        .iter()
        .filter(|event| {
            event.kind == "memory.created" && event.payload["title"] == "Task T4 verification"
        })
        .count();
    assert_eq!(created, 1);
    assert!(!events.iter().any(|event| event.kind == "memory.superseded"));
}

#[tokio::test]
async fn integration_resolution_memory_carries_evidence_and_supersedes() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    super::record_integration_resolution_memory(
        &engine,
        &session.id,
        "entry-1",
        Some("T5"),
        "4444444444444444",
        "Resolution verified.",
        "2 path(s): src/lib.rs, src/main.rs; resolver: kept both sides",
        "merged; integration checks passed",
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("integration"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let memory = &active[0];
    assert_eq!(memory.title, "Integration entry-1 resolution");
    assert_eq!(memory.source, "orchestrator");
    assert!(memory.body.starts_with("resolved: true"));
    assert!(memory.body.contains("verifier: Resolution verified."));
    assert!(memory.body.contains("commit: 4444444444444444"));
    assert!(memory.body.contains("conflict: 2 path(s)"));
    assert!(memory.body.contains("outcome: merged;"));
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| { evidence.kind == "session" && evidence.reference == session.id })
    );
    assert!(
        memory.evidence.iter().any(|evidence| {
            evidence.kind == "commit" && evidence.reference == "4444444444444444"
        })
    );
    assert!(
        memory
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "task" && evidence.reference == "T5")
    );

    super::record_integration_resolution_memory(
        &engine,
        &session.id,
        "entry-1",
        Some("T5"),
        "5555555555555555",
        "Resolution verified again.",
        "2 path(s): src/lib.rs, src/main.rs; resolver: retried resolution",
        "integration failed: session worktree is dirty",
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("integration"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    assert!(active[0].body.contains("Resolution verified again."));
    assert!(active[0].body.contains("integration failed"));
    assert_eq!(active[0].supersedes.len(), 1);
    let superseded = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("integration"),
            Some("superseded"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(superseded.len(), 1);
    assert!(superseded[0].body.contains("outcome: merged;"));
}

#[tokio::test]
async fn failed_integration_is_not_described_as_merged() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    super::record_verification_memory(
        &engine,
        &session.id,
        None,
        "ACP Resolver",
        "9999999999999999",
        &capture_verdict(true, "Approved but the merge preflight failed."),
        Some("integration failed: session worktree is dirty"),
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let memory = &active[0];
    assert_eq!(memory.title, "Worker ACP Resolver verification 99999999");
    assert!(memory.body.contains("approved: true"));
    assert!(memory.body.contains("integration: integration failed:"));
    assert!(!memory.body.contains("merged"));
    assert!(
        !memory
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "task")
    );
    assert!(
        memory.evidence.iter().any(|evidence| {
            evidence.kind == "commit" && evidence.reference == "9999999999999999"
        })
    );
}

#[tokio::test]
async fn task_scoped_verification_memory_appears_in_the_brief_once() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    super::record_verification_memory(
        &engine,
        &session.id,
        Some("T6"),
        "Worker 6",
        "6666666666666666",
        &capture_verdict(true, "Task-scoped verification passed."),
        Some("merged; checks passed"),
    )
    .await;
    let active = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(active.len(), 1);
    let memory_id = active[0].id.clone();
    let brief = engine.memory_brief(&session.id, Some("T6")).await.unwrap();
    assert!(brief.text.contains("Task T6 verification"));
    assert!(
        brief
            .memories
            .iter()
            .any(|memory| memory.memory.id == memory_id)
    );
    let used = engine.store().get_memory(&memory_id).unwrap().unwrap();
    assert_eq!(used.use_count, 1);
    engine.memory_brief(&session.id, Some("T6")).await.unwrap();
    let used_again = engine.store().get_memory(&memory_id).unwrap().unwrap();
    assert_eq!(used_again.use_count, 1);
    let events = engine
        .list_events(session.id.clone(), 0, 1_000)
        .await
        .unwrap();
    let used_events = events
        .iter()
        .filter(|event| {
            event.kind == "memory.used" && event.payload["title"] == "Task T6 verification"
        })
        .count();
    assert_eq!(used_events, 1);
}

#[tokio::test]
async fn unrelated_verification_does_not_outrank_a_same_task_result() {
    let (_data, _repo, engine, session) = memory_capture_fixture().await;
    super::record_verification_memory(
        &engine,
        &session.id,
        Some("T8"),
        "Worker 8",
        "8888888888888888",
        &capture_verdict(true, "Unrelated verification passed."),
        Some("merged; checks passed"),
    )
    .await;
    let unrelated = engine
        .store()
        .list_memories_filtered(
            &session.project_id,
            None,
            Some("verification"),
            Some("active"),
            false,
            10,
        )
        .unwrap();
    assert_eq!(unrelated.len(), 1);
    let task_result = engine
        .create_memory_with_metadata(
            &session.id,
            "worker-7",
            "Worker",
            "task_result",
            "Task T7 result",
            "Task T7 completed with deterministic tests.",
            vec!["T7".to_string(), "task_result".to_string()],
            "task_result",
            vec![
                vp_protocol::types::MemoryEvidence {
                    kind: "task".to_string(),
                    reference: "T7".to_string(),
                    note: None,
                },
                vp_protocol::types::MemoryEvidence {
                    kind: "session".to_string(),
                    reference: session.id.clone(),
                    note: None,
                },
            ],
            false,
            None,
        )
        .await
        .unwrap();
    let search = engine
        .memory_search(vp_protocol::types::MemorySearchParams {
            project_id: session.project_id.clone(),
            query: "verification T7".to_string(),
            kind: None,
            kinds: Vec::new(),
            task_id: Some("T7".to_string()),
            department: None,
            paths: Vec::new(),
            limit: Some(10),
            include_stale: false,
            max_chars: None,
            lexical_only: false,
        })
        .await
        .unwrap();
    assert_eq!(search.memories[0].memory.id, task_result.id);
    let unrelated_position = search
        .memories
        .iter()
        .position(|memory| memory.memory.id == unrelated[0].id);
    assert!(
        unrelated_position.is_none_or(|position| position > 0),
        "unrelated verification outranked the same-task result"
    );
}

fn recording_service(
    dir: &TempDir,
) -> (
    Engine,
    AgentService,
    vp_protocol::types::Project,
    Arc<Mutex<Vec<ChatRequest>>>,
) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let provider_trait: Arc<dyn Provider> = provider;
    let (engine, service, project) = test_service_with_provider(dir, provider_trait);
    (engine, service, project, requests)
}

#[tokio::test]
async fn prompt_operation_replays_without_a_second_run() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project, requests) = recording_service(&dir);
    let session = engine
        .create_session(project.id, "prompt operation".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    service
        .start_with_operation(&session.id, "repeatable instructions", Some("op-prompt-1"))
        .await
        .unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    assert_eq!(requests.lock().unwrap().len(), 1);

    service
        .start_with_operation(&session.id, "repeatable instructions", Some("op-prompt-1"))
        .await
        .unwrap();
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "replay must not call the provider again"
    );
    let messages = engine.list_messages(&session.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| {
                message.role == "user" && message.content == "repeatable instructions"
            })
            .count(),
        1,
        "replay must not append a second user message"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "agent.queued")
            .count(),
        1,
        "replay must not enqueue a second run"
    );

    let conflict = service
        .start_with_operation(&session.id, "different instructions", Some("op-prompt-1"))
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("different method or payload"));
    assert_eq!(requests.lock().unwrap().len(), 1);

    service
        .start_with_operation(&session.id, "repeatable instructions", Some("op-prompt-2"))
        .await
        .unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "a new operation id must start a new run"
    );
}

#[tokio::test]
async fn prompt_operation_in_progress_is_rejected() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project, requests) = recording_service(&dir);
    let session = engine
        .create_session(project.id, "pending operation".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let payload = serde_json::json!({
        "session_id": session.id,
        "content": "one run",
    });
    let fingerprint = crate::operation::fingerprint("session.prompt", &payload).unwrap();
    assert_eq!(
        engine
            .store()
            .claim_session_operation(
                &session.id,
                "op-in-progress",
                "session.prompt",
                &fingerprint
            )
            .unwrap(),
        Claim::New
    );

    let error = service
        .start_with_operation(&session.id, "one run", Some("op-in-progress"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already in progress"));
    assert_eq!(requests.lock().unwrap().len(), 0);
    assert!(engine.list_messages(&session.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn prompt_operation_unknown_after_recovery_is_rejected() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project, requests) = recording_service(&dir);
    let session = engine
        .create_session(project.id, "crash window".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let payload = serde_json::json!({
        "session_id": session.id,
        "content": "maybe started",
    });
    let fingerprint = crate::operation::fingerprint("session.prompt", &payload).unwrap();
    let store = engine.store().clone();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-unknown", "session.prompt", &fingerprint)
            .unwrap(),
        Claim::New
    );

    engine.recover_interrupted_sessions().await.unwrap();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-unknown", "session.prompt", &fingerprint)
            .unwrap(),
        Claim::Unknown
    );
    let error = service
        .start_with_operation(&session.id, "maybe started", Some("op-unknown"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown outcome"));
    assert_eq!(requests.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn prompt_operation_failed_can_be_retried_with_the_same_id() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project, requests) = recording_service(&dir);
    let session = engine
        .create_session(project.id, "failed operation".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let config = service.config.write().unwrap().take();
    let error = service
        .start_with_operation(&session.id, "retry me", Some("op-retry"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider is not configured"));
    *service.config.write().unwrap() = config;

    service
        .start_with_operation(&session.id, "retry me", Some("op-retry"))
        .await
        .unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    assert_eq!(requests.lock().unwrap().len(), 1);

    service
        .start_with_operation(&session.id, "retry me", Some("op-retry"))
        .await
        .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn program_tick_creates_one_session_and_never_duplicates_it() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let program = engine
        .create_program(vp_protocol::types::ProgramCreateParams {
            project_id: project.id.clone(),
            goal: "Program dispatch".into(),
            policy: Some(vp_protocol::types::ProgramPolicy {
                auto_dispatch: true,
                max_attempts: 2,
                retry_backoff_secs: 60,
                dependency_failure: vp_protocol::types::DependencyFailurePolicy::Block,
                token_budget: None,
                cost_budget_usd: None,
                max_parallel_milestones: 1,
            }),
            token_budget: None,
            cost_budget_usd: None,
            activate: true,
        })
        .await
        .unwrap();
    let milestone = engine
        .create_program_milestone(vp_protocol::types::ProgramMilestoneCreateParams {
            program_id: program.id.clone(),
            key: Some("1".into()),
            title: "Dispatch once".into(),
            objective: "Create exactly one normal session.".into(),
            acceptance: vec!["one session exists".into()],
            deps: Vec::new(),
            priority: Some(0),
            max_attempts: Some(2),
            profile: Some("coding".into()),
            write_scope: vec!["src".into()],
            token_budget: None,
            cost_budget_usd: None,
        })
        .await
        .unwrap();

    let tick = service.reconcile_programs(Some(&project.id)).await.unwrap();
    assert_eq!(tick.sessions_started, 1);
    let sessions = engine
        .list_sessions(Some(project.id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    let stored = engine
        .store()
        .get_program_milestone(&milestone.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.session_id.as_deref(), Some(sessions[0].id.as_str()));
    assert_eq!(stored.plan_revision, Some(1));

    let tick = service.reconcile_programs(Some(&project.id)).await.unwrap();
    assert_eq!(tick.sessions_started, 0);
    assert_eq!(
        engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap()
            .len(),
        1
    );
    service.cancel(&sessions[0].id);
}

#[tokio::test]
async fn cancelling_a_program_cancels_every_non_terminal_milestone() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let program = engine
        .create_program(vp_protocol::types::ProgramCreateParams {
            project_id: project.id.clone(),
            goal: "Cancel ledger".into(),
            policy: Some(vp_protocol::types::ProgramPolicy {
                auto_dispatch: false,
                max_attempts: 2,
                retry_backoff_secs: 60,
                dependency_failure: vp_protocol::types::DependencyFailurePolicy::Block,
                token_budget: None,
                cost_budget_usd: None,
                max_parallel_milestones: 2,
            }),
            token_budget: None,
            cost_budget_usd: None,
            activate: true,
        })
        .await
        .unwrap();
    let first = engine
        .create_program_milestone(vp_protocol::types::ProgramMilestoneCreateParams {
            program_id: program.id.clone(),
            key: Some("1".into()),
            title: "Pending work".into(),
            objective: "Never starts".into(),
            acceptance: vec!["done".into()],
            deps: Vec::new(),
            priority: Some(0),
            max_attempts: Some(2),
            profile: Some("coding".into()),
            write_scope: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
        })
        .await
        .unwrap();
    let second = engine
        .create_program_milestone(vp_protocol::types::ProgramMilestoneCreateParams {
            program_id: program.id.clone(),
            key: Some("2".into()),
            title: "Dependent work".into(),
            objective: "Never starts".into(),
            acceptance: vec!["done".into()],
            deps: vec![first.id.clone()],
            priority: Some(0),
            max_attempts: Some(2),
            profile: Some("coding".into()),
            write_scope: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
        })
        .await
        .unwrap();

    let cancelled = service
        .cancel_program(&program.id, "operator stop")
        .await
        .unwrap();
    assert_eq!(cancelled.status, vp_protocol::types::ProgramStatus::Failed);
    assert!(cancelled.reason.unwrap().contains("operator stop"));
    for milestone in [first, second] {
        let stored = engine
            .store()
            .get_program_milestone(&milestone.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.status,
            vp_protocol::types::ProgramMilestoneStatus::Cancelled
        );
        assert!(stored.last_error.unwrap().contains("program cancelled"));
    }
    let events = engine
        .store()
        .list_program_events(&program.id, Some(100))
        .unwrap();
    assert!(
        events
            .iter()
            .filter(|event| event.kind == "program.milestone.cancelled")
            .count()
            >= 2
    );
    assert!(events.iter().any(|event| event.kind == "program.failed"));
}

#[tokio::test]
async fn program_start_fails_before_claim_without_a_provider() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let program = engine
        .create_program(vp_protocol::types::ProgramCreateParams {
            project_id: project.id.clone(),
            goal: "No provider".into(),
            policy: Some(vp_protocol::types::ProgramPolicy {
                auto_dispatch: true,
                max_attempts: 2,
                retry_backoff_secs: 60,
                dependency_failure: vp_protocol::types::DependencyFailurePolicy::Block,
                token_budget: None,
                cost_budget_usd: None,
                max_parallel_milestones: 1,
            }),
            token_budget: None,
            cost_budget_usd: None,
            activate: true,
        })
        .await
        .unwrap();
    let milestone = engine
        .create_program_milestone(vp_protocol::types::ProgramMilestoneCreateParams {
            program_id: program.id.clone(),
            key: Some("1".into()),
            title: "Never claimed".into(),
            objective: "Do not spend an attempt".into(),
            acceptance: vec!["done".into()],
            deps: Vec::new(),
            priority: Some(0),
            max_attempts: Some(2),
            profile: Some("coding".into()),
            write_scope: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
        })
        .await
        .unwrap();
    let config = service.config.write().unwrap().take();
    let error = service
        .start_program_milestone(&milestone.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider is not configured"));
    assert!(service.config.write().unwrap().is_none());
    *service.config.write().unwrap() = config;
    let stored = engine
        .store()
        .get_program_milestone(&milestone.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.status,
        vp_protocol::types::ProgramMilestoneStatus::Pending
    );
    assert_eq!(stored.attempts, 0);
    assert!(stored.session_id.is_none());
}

#[test]
fn remote_tools_are_explicit_and_role_scoped() {
    for tool in [
        "remote_read",
        "remote_write",
        "remote_list",
        "remote_exec",
        "remote_git",
        "remote_worktree",
    ] {
        assert!(
            crate::agent::AgentRole::Leader.allows_tool(crate::agent::AgentProfile::Coding, tool)
        );
    }
    assert!(
        crate::agent::AgentRole::Manager
            .allows_tool(crate::agent::AgentProfile::Coding, "remote_read")
    );
    assert!(
        crate::agent::AgentRole::Manager
            .allows_tool(crate::agent::AgentProfile::Coding, "remote_git")
    );
    assert!(
        !crate::agent::AgentRole::Manager
            .allows_tool(crate::agent::AgentProfile::Coding, "remote_write")
    );
    assert!(
        crate::agent::AgentRole::Worker
            .allows_tool(crate::agent::AgentProfile::Coding, "remote_worktree")
    );
    assert!(
        !crate::agent::AgentRole::Worker
            .allows_tool(crate::agent::AgentProfile::Research, "remote_worktree")
    );
    assert!(
        !crate::agent::AgentRole::Verifier
            .allows_tool(crate::agent::AgentProfile::Coding, "remote_write")
    );
}

#[allow(clippy::too_many_arguments)]
async fn begin_durable_journal(
    engine: &Engine,
    session: &vp_protocol::types::Session,
    project_id: &str,
    objective: &str,
    provider: &str,
    model: &str,
    top_level: bool,
    task_id: Option<&str>,
) -> crate::run_journal::RunJournal {
    crate::run_journal::RunJournal::begin(
        engine,
        session,
        project_id,
        "leader-id",
        "Leader",
        "leader",
        task_id,
        None,
        objective,
        provider,
        model,
        Path::new(session.worktree.as_deref().expect("session has a worktree")),
        RunBudgetSnapshot::default(),
        top_level,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_completed_leader_run_is_journaled_and_replays_consistently() {
    let dir = TempDir::new().unwrap();
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
                StreamEvent::TextDelta("journalled and done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "journal a run".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "journal a run").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");

    let runs = engine.session_runs(&session.id, 20).await.unwrap().runs;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "completed");
    assert!(runs[0].top_level);
    assert_eq!(runs[0].agent_role, "leader");
    let detail = engine.session_run_detail(&runs[0].run_id).await.unwrap();
    assert!(
        detail
            .steps
            .iter()
            .any(|step| step.kind == "model_request" && step.status == "completed")
    );
    assert!(
        detail
            .steps
            .iter()
            .any(|step| step.kind == "tool_call" && step.status == "completed")
    );
    assert!(!detail.checkpoints.is_empty());
    assert_eq!(detail.tool_calls.len(), 1);
    assert_eq!(detail.tool_calls[0].status, "completed");

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(report.runs_match);
    assert!(report.run_steps_match);
    assert!(report.run_checkpoints_match);
}

#[tokio::test]
async fn resumes_a_crashed_run_and_hard_blocks_a_completed_write() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call-dup".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"journal-duplicate.txt","content":"should-not-run"}"#
                        .into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("resume finished".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "crash and resume".into(), Some(100_000))
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();

    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "crash and resume",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let input = serde_json::json!({"path": "journal-duplicate.txt", "content": "should-not-run"});
    let (step, tool, _) = journal
        .tool_intent(&engine, 1, "call-prior", "write_file", &input, None)
        .await
        .unwrap();
    journal
        .tool_completed(
            &engine,
            &step,
            &tool,
            &ToolOutput::ok("wrote journal-duplicate.txt (prior attempt)"),
        )
        .await
        .unwrap();
    let prior_run_id = journal.id().to_string();
    drop(journal);
    engine.recover_interrupted_sessions().await.unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();

    let resumed = service
        .resume_session_run(&session.id, Some(&prior_run_id), None)
        .await
        .unwrap();
    assert!(resumed.resumed);
    assert_eq!(resumed.attempt, 2);
    assert_eq!(
        resumed.resumed_from_run_id.as_deref(),
        Some(prior_run_id.as_str())
    );
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");

    let path = Path::new(session.worktree.as_deref().unwrap()).join("journal-duplicate.txt");
    assert!(
        !path.exists(),
        "the journaled write must be returned, never executed a second time"
    );
    let runs = engine.session_runs(&session.id, 20).await.unwrap().runs;
    assert_eq!(runs.len(), 2);
    let old = runs.iter().find(|run| run.run_id == prior_run_id).unwrap();
    assert_eq!(old.status, "resumed");
    let new = runs
        .iter()
        .find(|run| run.run_id == resumed.run_id)
        .unwrap();
    assert_eq!(new.status, "completed");
    let old_detail = engine.session_run_detail(&prior_run_id).await.unwrap();
    assert_eq!(
        old_detail.tool_calls[0].consumed_by_run_id.as_deref(),
        Some(resumed.run_id.as_str())
    );
    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(
            captured[1]
                .messages
                .iter()
                .any(|message| message.content.contains("[Durable run journal]"))
        );
        assert!(
            captured[1]
                .messages
                .iter()
                .any(|message| message.content.contains("[durable journal]"))
        );
    }
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
}

#[tokio::test]
async fn resume_reconstructs_persisted_assistant_messages_from_events() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("continued after event replay".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "event replay".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
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
            "original request",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "event replay",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let request = ChatRequest::new("test", vec![ChatMessage::user("original request")]);
    let _intent = journal.model_intent(&engine, 1, &request).await.unwrap();
    engine
        .append_message(
            &session.id,
            "assistant",
            "persisted assistant response",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let prior_run = journal.id().to_string();
    drop(journal);
    engine.recover_interrupted_sessions().await.unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    service
        .resume_session_run(&session.id, Some(&prior_run), None)
        .await
        .unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let contents = captured[0]
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert!(contents.contains(&"persisted assistant response"));
    assert!(
        contents
            .iter()
            .any(|content| content.contains("Durable run journal"))
    );
}

#[tokio::test]
async fn auto_resume_is_opt_in_and_bounded_by_the_project_policy() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("auto resumed".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "auto resume".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "auto resume",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let run_id = journal.id().to_string();
    drop(journal);
    engine.recover_interrupted_sessions().await.unwrap();
    // Opt-in is off by default.
    assert_eq!(service.auto_resume_interrupted_runs().await.unwrap(), 0);
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: true,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    assert_eq!(service.auto_resume_interrupted_runs().await.unwrap(), 1);
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let runs = engine.session_runs(&session.id, 20).await.unwrap().runs;
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter()
            .any(|run| run.run_id == run_id && run.status == "resumed")
    );
    assert!(
        runs.iter()
            .any(|run| run.status == "completed" && run.attempt == 2)
    );
    // A second tick must not create an additional attempt.
    assert_eq!(service.auto_resume_interrupted_runs().await.unwrap(), 0);
    assert_eq!(
        engine
            .session_runs(&session.id, 20)
            .await
            .unwrap()
            .runs
            .len(),
        2
    );
}

#[tokio::test]
async fn cooperative_cancel_finishes_the_journal_as_cancelled_and_blocks_resume() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(GatedFinalProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "cancel me".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "cancel me").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    let runs = engine.session_runs(&session.id, 10).await.unwrap().runs;
    let running = runs.iter().find(|run| run.status == "running").unwrap();
    let result = service
        .cancel_session_run(&session.id, Some(&running.run_id))
        .await
        .unwrap();
    assert!(result.cancelled);
    provider.release.notify_one();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let runs = engine.session_runs(&session.id, 10).await.unwrap().runs;
    let cancelled = runs
        .iter()
        .find(|run| run.run_id == running.run_id)
        .unwrap();
    assert_eq!(cancelled.status, "interrupted");
    assert_eq!(
        cancelled
            .failure
            .as_ref()
            .map(|failure| failure.kind.as_str()),
        Some("cancelled")
    );
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    let error = service
        .resume_session_run(&session.id, Some(&cancelled.run_id), None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cancelled"));
}

#[tokio::test]
async fn resume_operation_receipts_replay_instead_of_starting_a_second_attempt() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("one resume".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "resume receipt".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "resume receipt",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let prior = journal.id().to_string();
    drop(journal);
    engine.recover_interrupted_sessions().await.unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    let first = service
        .resume_session_run(&session.id, Some(&prior), Some("op-resume-once"))
        .await
        .unwrap();
    let second = service
        .resume_session_run(&session.id, Some(&prior), Some("op-resume-once"))
        .await
        .unwrap();
    assert_eq!(first.run_id, second.run_id);
    let _finished = wait_until_idle(&engine, &session).await;
    let runs = engine.session_runs(&session.id, 20).await.unwrap().runs;
    assert_eq!(
        runs.len(),
        2,
        "receipt replay must not create another attempt"
    );
}

#[tokio::test]
async fn resume_fills_missing_tool_results_for_a_persisted_multi_call_assistant_turn() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("resumed multi-call".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "multi call".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
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
            "do two things",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let assistant_calls = vec![
        ToolCall {
            id: "call-write-missing".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"recovered.txt","content":"one"}"#.into(),
        },
        ToolCall {
            id: "call-shell-missing".into(),
            name: "shell".into(),
            arguments: r#"{"command":"touch maybe"}"#.into(),
        },
    ];
    engine
        .append_message(
            &session.id,
            "assistant",
            "I will use two tools.",
            serde_json::to_value(&assistant_calls).unwrap(),
            None,
        )
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "multi call",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let (write_step, write_tool, _) = journal
        .tool_intent(
            &engine,
            1,
            "call-write-missing",
            "write_file",
            &serde_json::json!({"path": "recovered.txt", "content": "one"}),
            None,
        )
        .await
        .unwrap();
    journal
        .tool_completed(
            &engine,
            &write_step,
            &write_tool,
            &ToolOutput::ok("wrote recovered.txt"),
        )
        .await
        .unwrap();
    let _ = journal
        .tool_intent(
            &engine,
            1,
            "call-shell-missing",
            "shell",
            &serde_json::json!({"command": "touch maybe"}),
            None,
        )
        .await
        .unwrap();
    let prior = journal.id().to_string();
    drop(journal);
    engine.recover_interrupted_sessions().await.unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    service
        .resume_session_run(&session.id, Some(&prior), None)
        .await
        .unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let tool_messages = captured[0]
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .map(|message| {
            (
                message.tool_call_id.clone().unwrap_or_default(),
                message.content.clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 2);
    let write = tool_messages
        .iter()
        .find(|(id, _)| id == "call-write-missing")
        .unwrap();
    assert!(write.1.contains("wrote recovered.txt"));
    assert!(write.1.contains("not re-run"));
    let shell = tool_messages
        .iter()
        .find(|(id, _)| id == "call-shell-missing")
        .unwrap();
    assert!(shell.1.contains("Unknown outcome"));
    assert!(shell.1.contains("Verify"));
}

#[tokio::test]
async fn resume_without_a_run_id_selects_the_newest_resumable_top_level_run() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("newest resumed".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "newest run".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let first = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "older",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let first_id = first.id().to_string();
    first
        .finish_interrupted(&engine, "interrupted", "old crash")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2)).await;
    let second = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "newer",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let second_id = second.id().to_string();
    second
        .finish_interrupted(&engine, "interrupted", "new crash")
        .await
        .unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    let resumed = service
        .resume_session_run(&session.id, None, None)
        .await
        .unwrap();
    assert_eq!(
        resumed.resumed_from_run_id.as_deref(),
        Some(second_id.as_str())
    );
    let _ = wait_until_idle(&engine, &session).await;
    let runs = engine.session_runs(&session.id, 20).await.unwrap().runs;
    let older = runs.iter().find(|run| run.run_id == first_id).unwrap();
    assert_eq!(older.status, "interrupted");
    let newer = runs.iter().find(|run| run.run_id == second_id).unwrap();
    assert_eq!(newer.status, "resumed");
}

#[tokio::test]
async fn task_scoped_runs_are_journaled_but_not_resumed_adhoc() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "task scoped".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "task scoped",
        "queue",
        "test",
        false,
        Some("T1"),
    )
    .await;
    let run_id = journal.id().to_string();
    journal
        .finish_interrupted(&engine, "interrupted", "worker crash")
        .await
        .unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    let error = service
        .resume_session_run(&session.id, Some(&run_id), None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("task-scoped"));
    let runs = engine.session_runs(&session.id, 10).await.unwrap().runs;
    assert_eq!(runs[0].status, "interrupted");
}

#[tokio::test]
async fn resume_requires_a_configured_provider() {
    let dir = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(dir.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("unconfigured".to_string()),
        )
        .await
        .unwrap();
    let mut session = engine
        .create_session(project.id, "no provider".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "no provider",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let run_id = journal.id().to_string();
    journal
        .finish_interrupted(&engine, "interrupted", "crash")
        .await
        .unwrap();
    let service = AgentService::unconfigured(engine.clone());
    let error = service
        .resume_session_run(&session.id, Some(&run_id), None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("not configured"));
}

#[tokio::test]
async fn cancel_without_a_running_run_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "nothing running".into(), None)
        .await
        .unwrap();
    let result = service.cancel_session_run(&session.id, None).await.unwrap();
    assert!(!result.cancelled);
    assert!(result.run_id.is_none());
    assert!(result.detail.contains("no running"));
}

#[tokio::test]
async fn resumed_read_only_tools_re_run_instead_of_returning_a_journaled_result() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call-read-again".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("read after resume".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "read again".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let journal = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "read again",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let (step, tool, _) = journal
        .tool_intent(
            &engine,
            1,
            "call-read-again",
            "read_file",
            &serde_json::json!({"path": "README.md"}),
            None,
        )
        .await
        .unwrap();
    journal
        .tool_completed(
            &engine,
            &step,
            &tool,
            &ToolOutput::ok("STALE JOURNALED CONTENT"),
        )
        .await
        .unwrap();
    let prior = journal.id().to_string();
    drop(journal);
    engine.recover_interrupted_sessions().await.unwrap();
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    service
        .resume_session_run(&session.id, Some(&prior), None)
        .await
        .unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let captured = requests.lock().unwrap();
    let tool_result = captured[1]
        .messages
        .iter()
        .find(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("call-read-again")
        })
        .expect("the resumed provider request contains the tool result");
    assert!(tool_result.content.contains("hello from test"));
    assert!(!tool_result.content.contains("STALE JOURNALED CONTENT"));
    assert!(!tool_result.content.contains("[durable journal]"));
}

#[tokio::test]
async fn a_new_prompt_supersedes_an_older_unresolved_run() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("new prompt completed".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut session = engine
        .create_session(project.id, "supersede".into(), None)
        .await
        .unwrap();
    session.status = "idle".to_string();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let old = begin_durable_journal(
        &engine,
        &session,
        &session.project_id,
        "crashed",
        "queue",
        "test",
        true,
        None,
    )
    .await;
    let old_id = old.id().to_string();
    old.finish_interrupted(&engine, "interrupted", "crash")
        .await
        .unwrap();
    service.start(&session.id, "new prompt").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let runs = engine.session_runs(&session.id, 20).await.unwrap().runs;
    let old = runs.iter().find(|run| run.run_id == old_id).unwrap();
    assert_eq!(
        old.failure.as_ref().map(|failure| failure.kind.as_str()),
        Some("superseded")
    );
    engine
        .set_session_run_policy(
            &session.project_id,
            vp_protocol::types::SessionRunPolicy {
                auto_resume: false,
                max_attempts: 2,
                max_age_hours: 24,
            },
        )
        .await
        .unwrap();
    let error = service
        .resume_session_run(&session.id, Some(&old_id), None)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("superseded"));
}

#[tokio::test]
async fn stale_integration_owner_aborts_before_touching_the_session_branch() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let engine = Engine::new(store.clone());
    let other = Engine::new(Store::open(dir.path()).unwrap());
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "initial\n").unwrap();
    init_repo(&repo);
    let project = engine
        .add_project(repo.to_string_lossy().to_string(), Some("fence".into()))
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "fenced merge".into(), None)
        .await
        .unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    let session_branch = session.branch.clone().unwrap();
    let session_head = worktree::head_commit(&session_worktree).unwrap();
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
    std::fs::write(task.path.join("worker.txt"), "from stale worker\n").unwrap();
    let commit = worktree::commit_all(&task.path, "worker change")
        .unwrap()
        .unwrap();
    let entry = engine
        .enqueue_merge(
            &session.id,
            "worker-stale",
            "Worker stale",
            Some("T1".into()),
            &task.branch,
            &commit,
            &session_head,
            Some("verified".into()),
        )
        .await
        .unwrap();

    // The first generation is already expired. A second process has taken
    // over the key, so the old guard must fail its effect-boundary assertion.
    let stale = engine
        .acquire_runtime_lease_at_for_test(
            "integration",
            &session.id,
            0,
            time::OffsetDateTime::now_utc() - time::Duration::seconds(300),
        )
        .await
        .unwrap();
    let current = other
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(current.generation(), stale.generation() + 1);

    let spec = AgentSpec {
        id: "worker-stale".into(),
        role: AgentRole::Worker,
        name: "Worker stale".into(),
        depth: 2,
        parent_name: Some("Manager".into()),
        title: None,
        task_id: Some("T1".into()),
        department: Some("impl".into()),
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: true,
        task_kind: None,
        auto_approve: false,
        worktree: task.path.clone(),
        branch: task.branch.clone(),
        session_worktree: session_worktree.clone(),
        session_branch: session_branch.clone(),
        write_scope: vec!["worker.txt".into()],
        base_commit: task.base_commit.clone(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let error = process_merge_entry_fenced(&engine, &session, &spec, &entry, None, &stale)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("lease lost before merge"),
        "{error}"
    );
    assert_eq!(
        worktree::head_commit(&session_worktree).unwrap(),
        session_head,
        "a stale integrator must not change the session branch"
    );
    assert!(!session_worktree.join("worker.txt").exists());
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, "failed");
    assert!(
        queue[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("lease lost before merge"))
    );
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "integrator.finished" && event.payload["reason"] == "lease_lost_before_merge"
    }));
    // Dropping the stale guard must not delete the newer generation.
    drop(stale);
    let leases = other.list_runtime_leases().await.unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].owner_id, other.runtime_id());
    current.release().await;
}

fn configured_service_for_test(
    engine: Engine,
    provider: Arc<dyn Provider>,
    label: &str,
) -> AgentService {
    let config = AgentConfig {
        provider,
        provider_name: label.into(),
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
        max_active_sessions: 4,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(
            global_worker_cap(),
            global_labour_cap(),
        )),
        provider_idle_timeout_secs: 300,
        question_timeout_secs: 1800,
        approval_mode: ApprovalMode::default(),
        approval_timeout_secs: 600,
        compact_after_tokens: 0,
        compact_keep_messages: 8,
        compact_keep_tokens: 15_000,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    };
    AgentService::configured(engine, config)
}

#[tokio::test]
async fn two_continuation_reconcilers_deliver_one_child() {
    let dir = TempDir::new().unwrap();
    let provider_a: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("one delivery".into()),
            StreamEvent::Done,
        ]])),
    });
    let provider_b: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service_a, project) = test_service_with_provider(&dir, provider_a);
    let engine_b = Engine::new(Store::open(dir.path()).unwrap());
    let service_b = configured_service_for_test(engine_b.clone(), provider_b, "queue-b");
    let session = engine
        .create_session(project.id, "two reconcilers".into(), None)
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "deliver exactly once",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();

    // The claim primitive is independently race-tested. Here we verify the
    // full reconciler path: the first pass delivers one child, and the second
    // process (sharing the data directory) sees no open row.
    assert_eq!(
        service_a.reconcile_session_continuations().await.unwrap(),
        1
    );
    assert_eq!(
        service_b.reconcile_session_continuations().await.unwrap(),
        0
    );
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivering")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.scheduled")
            .count(),
        0
    );
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "delivered");
    assert_eq!(rows[0].run_count, 1);
    let _ = wait_until_idle(&engine, &session).await;
    assert_eq!(
        service_a.reconcile_session_continuations().await.unwrap(),
        0
    );
    assert_eq!(
        service_b.reconcile_session_continuations().await.unwrap(),
        0
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

fn set_context_window(
    service: &AgentService,
    context_window: u64,
    compact_after_tokens: u64,
    keep_messages: usize,
    keep_tokens: u64,
) {
    let mut config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service is configured")
        .clone();
    config.context_window = context_window;
    config.compact_after_tokens = compact_after_tokens;
    config.compact_keep_messages = keep_messages;
    config.compact_keep_tokens = keep_tokens;
    *service.config.write().unwrap() = Some(config);
}

async fn append_context_messages(engine: &Engine, session_id: &str, count: usize, content: &str) {
    for index in 0..count {
        engine
            .append_message(
                session_id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: {content}"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
}

async fn set_session_idle(engine: &Engine, session_id: &str) {
    engine
        .update_session_status(session_id, "idle")
        .await
        .unwrap();
}

async fn first_context_checkpoint(engine: &Engine, session_id: &str) -> ContextCheckpoint {
    engine
        .list_context_checkpoints(session_id, Some(200))
        .await
        .unwrap()
        .checkpoints
        .into_iter()
        .next()
        .expect("a context checkpoint")
}

#[tokio::test]
async fn automatic_compaction_creates_a_durable_context_checkpoint() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("automatic checkpoint summary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 3, 1_000_000);
    let session = engine
        .create_session(project.id, "automatic checkpoint".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(
        &engine,
        &session.id,
        14,
        "the release pipeline investigation includes enough detail to exceed the tiny test threshold",
    )
    .await;
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);

    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let compacted = events
        .iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted");
    let checkpointed = events
        .iter()
        .find(|event| event.kind == "context.checkpointed")
        .expect("context.checkpointed");
    let checkpoint =
        crate::context::context_checkpoint_from_event(checkpointed).expect("checkpoint payload");

    assert!(checkpoint.automatic);
    assert_eq!(checkpoint.source_compaction_seq, Some(compacted.seq));
    assert_eq!(
        checkpoint.summary.as_deref(),
        compacted.payload["summary"].as_str()
    );
    assert_eq!(
        checkpoint.through_message_id.as_deref(),
        compacted.payload["through_message_id"].as_str()
    );
    assert_eq!(
        checkpoint.tokens_before,
        compacted.payload["estimated_tokens_before"]
            .as_u64()
            .unwrap()
    );
    assert_eq!(
        checkpoint.tokens_after,
        compacted.payload["estimated_tokens_after"]
            .as_u64()
            .unwrap()
    );
    assert_eq!(
        compacted.payload["context_checkpoint_id"].as_str(),
        Some(checkpoint.id.as_str())
    );
    assert!(checkpoint.message_cursor > 0);
    assert!(!checkpoint.worktree_head.is_empty());
    assert_eq!(checkpoint.provider, "queue");
    assert_eq!(checkpoint.model, "test");
    assert_eq!(
        checkpoint
            .structured_state
            .as_ref()
            .map(|state| state.plan_revision),
        Some(checkpoint.plan_revision)
    );

    let listed = engine
        .list_context_checkpoints(&session.id, Some(20))
        .await
        .unwrap();
    assert_eq!(listed.checkpoints.len(), 1);
    assert_eq!(listed.latest_id.as_deref(), Some(checkpoint.id.as_str()));

    let status = engine.session_context_status(&session.id).await.unwrap();
    assert_eq!(status.checkpoint_count, 1);
    assert_eq!(
        status.latest_checkpoint_id.as_deref(),
        Some(checkpoint.id.as_str())
    );
    assert!(status.latest_checkpoint_at.is_some());
}

#[tokio::test]
async fn manual_context_checkpoint_captures_the_active_boundary() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "manual checkpoint".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 6, "manual boundary content").await;
    set_session_idle(&engine, &session.id).await;

    let checkpoint = service
        .context_checkpoint_now(&session.id, Some("before the refactor".into()))
        .await
        .unwrap();
    assert_eq!(checkpoint.label, "before the refactor");
    assert_eq!(checkpoint.mode, "manual");
    assert!(!checkpoint.automatic);
    assert!(checkpoint.summary.is_none());
    assert!(checkpoint.through_message_id.is_none());
    assert_eq!(checkpoint.plan_revision, 0);
    assert!(checkpoint.message_cursor == 0);
    assert!(checkpoint.tokens_before > 0);
    assert_eq!(checkpoint.tokens_before, checkpoint.tokens_after);
    assert!(!checkpoint.worktree_head.is_empty());
    assert_eq!(checkpoint.provider, "queue");
    assert_eq!(checkpoint.model, "test");

    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(detail.preflight.restorable, "{:?}", detail.preflight.errors);
    assert!(!detail.is_active);
    assert!(detail.summary_preview.is_empty());

    // A manual checkpoint after a compaction captures that exact boundary.
    let second_dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("manual summary boundary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine2, service2, project2) = test_service_with_provider(&second_dir, provider);
    set_context_window(&service2, 32_000, 1_000, 3, 1_000_000);
    let session2 = engine2
        .create_session(project2.id, "manual after compaction".into(), None)
        .await
        .unwrap();
    engine2
        .create_leader_agent(&session2.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine2, &session2.id, 12, "compacted content").await;
    service2.compact_now(&session2.id).await.unwrap();
    set_session_idle(&engine2, &session2.id).await;
    let manual = service2
        .context_checkpoint_now(&session2.id, None)
        .await
        .unwrap();
    assert_eq!(manual.mode, "summary");
    assert!(manual.summary.is_some());
    assert!(manual.through_message_id.is_some());
    assert!(!manual.automatic);
    assert!(manual.source_compaction_seq.is_some());
}

#[tokio::test]
async fn restore_rewinds_active_context_and_keeps_originals_searchable() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("first boundary".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second boundary".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id.clone(), "restore boundary".into(), None)
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
            "original-needle: keep this searchable after compaction",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, "first round content").await;
    assert!(service.compact_now(&session.id).await.unwrap().compacted);
    let checkpoints = engine
        .list_context_checkpoints(&session.id, Some(20))
        .await
        .unwrap();
    let first = checkpoints.checkpoints[0].clone();
    append_context_messages(&engine, &session.id, 10, "second round content").await;
    assert!(service.compact_now(&session.id).await.unwrap().compacted);
    let checkpoints = engine
        .list_context_checkpoints(&session.id, Some(20))
        .await
        .unwrap();
    assert_eq!(checkpoints.checkpoints.len(), 2);
    let second = checkpoints.checkpoints[1].clone();
    assert_ne!(first.id, second.id);

    set_session_idle(&engine, &session.id).await;
    let messages_before = engine.list_messages(&session.id).await.unwrap().len();
    let head_before = worktree::head_commit(&PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .clone()
            .unwrap(),
    ))
    .unwrap();

    let restored = service
        .restore_context_checkpoint_now(&session.id, &first.id, true)
        .await
        .unwrap();
    assert_eq!(restored.checkpoint_id, first.id);
    assert_eq!(restored.mode, "summary");
    assert!(
        restored.warning.is_some(),
        "the boundary estimate is above the low test threshold and force accepted it"
    );
    assert!(restored.estimated_tokens <= restored.hard_limit_tokens);

    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert!(
        loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
    assert!(loaded.messages[0].content.contains("first boundary"));
    assert!(!loaded.messages[0].content.contains("second boundary"));
    let messages_after = engine.list_messages(&session.id).await.unwrap().len();
    assert_eq!(
        messages_before, messages_after,
        "restore never rewrites message rows"
    );
    let head_after = worktree::head_commit(&PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .clone()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(
        head_before, head_after,
        "context restore never resets the worktree"
    );
    let session_after = engine.store().get_session(&session.id).unwrap().unwrap();
    assert_eq!(session_after.status, "idle");

    let hits = engine
        .search("original-needle", Some(&project.id), 10)
        .await
        .unwrap();
    assert!(hits.hits.iter().any(|hit| hit.session_id == session.id));

    // Idempotent: the same restore produces the same active context and a
    // second audit event, without duplicate message rows.
    let restored_again = service
        .restore_context_checkpoint_now(&session.id, &first.id, true)
        .await
        .unwrap();
    assert_eq!(restored_again.estimated_tokens, restored.estimated_tokens);
    assert_eq!(restored_again.active_messages, restored.active_messages);
    let loaded_again = load_conversation(&engine, &session.id).await.unwrap();
    assert_eq!(
        loaded
            .messages
            .iter()
            .map(|message| (message.role.clone(), message.content.clone()))
            .collect::<Vec<_>>(),
        loaded_again
            .messages
            .iter()
            .map(|message| (message.role.clone(), message.content.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        messages_before
    );
    let restore_events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "context.restored")
        .count();
    assert_eq!(restore_events, 2);

    // No agent run or merge was created by either restore.
    let kinds = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(!kinds.iter().any(|event| event.kind == "agent.run.started"));
    assert!(
        !kinds
            .iter()
            .any(|event| event.kind == "merge_queue.enqueued")
    );
}

#[tokio::test]
async fn restore_accepts_a_clean_descendant_without_resetting_the_worktree() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("descendant boundary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id, "descendant restore".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, "descendant content").await;
    service.compact_now(&session.id).await.unwrap();
    let checkpoint = first_context_checkpoint(&engine, &session.id).await;
    set_session_idle(&engine, &session.id).await;
    let worktree = PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .clone()
            .unwrap(),
    );
    std::fs::write(worktree.join("after-checkpoint.txt"), "kept\n").unwrap();
    let status = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["add", "."])
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["commit", "-q", "-m", "after checkpoint"])
        .status()
        .unwrap();
    assert!(status.success());
    let head = worktree::head_commit(&worktree).unwrap();

    let restored = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap();
    assert_eq!(restored.checkpoint_id, checkpoint.id);
    assert_eq!(worktree::head_commit(&worktree).unwrap(), head);
    assert!(worktree.join("after-checkpoint.txt").is_file());
    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(detail.preflight.worktree_descendant);
    assert!(detail.is_active);
    assert!(detail.preflight.restorable, "{:?}", detail.preflight.errors);
}

#[tokio::test]
async fn restore_refuses_plan_revision_model_provider_and_dirty_worktree_changes() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "mismatch restore".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 4, "mismatch content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();

    // Plan revision mismatch.
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("plan revision changed"),
        "{error}"
    );

    // Restore the plan state by capturing a matching checkpoint after the
    // revision. The original checkpoint must still refuse.
    let matching = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    assert_eq!(matching.plan_revision, 1);

    // Model mismatch.
    let original_model = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .model
        .clone();
    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.model = "different-model".into();
        *service.config.write().unwrap() = Some(config);
    }
    let error = service
        .restore_context_checkpoint_now(&session.id, &matching.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("model changed"), "{error}");
    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.model = original_model;
        *service.config.write().unwrap() = Some(config);
    }

    // Provider mismatch.
    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.provider_name = "another-provider".into();
        *service.config.write().unwrap() = Some(config);
    }
    let error = service
        .restore_context_checkpoint_now(&session.id, &matching.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider changed"), "{error}");
    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.provider_name = "queue".into();
        *service.config.write().unwrap() = Some(config);
    }

    // Dirty worktree.
    let worktree = PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .clone()
            .unwrap(),
    );
    std::fs::write(worktree.join("dirty.txt"), "uncommitted\n").unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &matching.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted changes"), "{error}");
    std::fs::remove_file(worktree.join("dirty.txt")).unwrap();
    service
        .restore_context_checkpoint_now(&session.id, &matching.id, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn restore_refuses_rehydrated_imported_and_exhausted_budget_sessions() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "import guard".into(), Some(1))
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 4, "import guard content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();

    // Rehydrated sessions carry a session.rehydrated event.
    engine
        .append_event(&session.id, "session.rehydrated", serde_json::json!({}))
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("imported or rehydrated"),
        "{error}"
    );

    // Imported status is rejected before any provider call.
    engine
        .update_session_status(&session.id, "imported")
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only archives"), "{error}");

    // Exhausted token budget on a fresh session/checkpoint.
    let budget_session = engine
        .create_session(project.id, "budget guard".into(), Some(1))
        .await
        .unwrap();
    engine
        .create_leader_agent(&budget_session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &budget_session.id, 3, "budget content").await;
    set_session_idle(&engine, &budget_session.id).await;
    let budget_checkpoint = service
        .context_checkpoint_now(&budget_session.id, None)
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &budget_session.id,
            "leader",
            "Leader",
            "leader",
            None,
            None,
            "test",
            5,
            5,
            false,
            0.0,
        )
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&budget_session.id, &budget_checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("budget is exhausted"), "{error}");
}

#[tokio::test]
async fn restore_hard_limit_is_absolute_and_force_only_bypasses_the_threshold_warning() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "hard limit".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, &"x".repeat(2_000)).await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();

    // Above the hard limit: force cannot bypass it.
    set_context_window(&service, 2_000, 0, 4, 1_000_000);
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("hard limit"), "{error}");

    // Between threshold and hard limit: refuse without force, accept with it.
    set_context_window(&service, 32_000, 1, 4, 1_000_000);
    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(
        detail.preflight.restorable,
        "threshold is a warning, not a safety error: {:?}",
        detail.preflight.errors
    );
    assert!(!detail.preflight.warnings.is_empty());
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("pass force=true"), "{error}");
    let restored = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap();
    assert!(restored.warning.is_some());
    assert!(restored.estimated_tokens <= restored.hard_limit_tokens);
}

#[tokio::test]
async fn delete_prune_and_retention_protect_the_latest_active_checkpoint() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "retention".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 4, "retention content").await;
    set_session_idle(&engine, &session.id).await;
    let first = service
        .context_checkpoint_now(&session.id, Some("first".into()))
        .await
        .unwrap();
    let second = service
        .context_checkpoint_now(&session.id, Some("second".into()))
        .await
        .unwrap();
    let third = service
        .context_checkpoint_now(&session.id, Some("third".into()))
        .await
        .unwrap();
    let message_count = engine.list_messages(&session.id).await.unwrap().len();

    let deleted = engine
        .delete_context_checkpoint(&session.id, &first.id)
        .await
        .unwrap();
    assert!(deleted.removed);
    let again = engine
        .delete_context_checkpoint(&session.id, &first.id)
        .await
        .unwrap();
    assert!(!again.removed);
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        message_count
    );
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "context.checkpoint.deleted"
                && event.payload["checkpoint_id"] == first.id)
    );

    // Count-only prune: keep the latest one.
    let pruned = engine
        .prune_context_checkpoints(&session.id, Some(0), Some(1), false)
        .await
        .unwrap();
    assert_eq!(pruned.removed, 1);
    assert_eq!(pruned.candidates, 1);
    let remaining = engine
        .list_context_checkpoints(&session.id, Some(20))
        .await
        .unwrap();
    assert_eq!(remaining.checkpoints.len(), 1);
    assert_eq!(remaining.checkpoints[0].id, third.id);
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        message_count
    );

    // Recreate two more checkpoints, then mark the session active. The latest
    // checkpoint of an active run is protected even with keep_latest = 0.
    let fourth = service
        .context_checkpoint_now(&session.id, Some("fourth".into()))
        .await
        .unwrap();
    let fifth = service
        .context_checkpoint_now(&session.id, Some("fifth".into()))
        .await
        .unwrap();
    assert_ne!(second.id, fourth.id);
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let error = engine
        .delete_context_checkpoint(&session.id, &fifth.id)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("latest context checkpoint"),
        "{error}"
    );
    let pruned = engine
        .prune_context_checkpoints(&session.id, Some(0), Some(0), false)
        .await
        .unwrap();
    assert_eq!(pruned.removed, 2);
    let remaining = engine
        .list_context_checkpoints(&session.id, Some(20))
        .await
        .unwrap();
    assert_eq!(remaining.checkpoints.len(), 1);
    assert_eq!(remaining.checkpoints[0].id, fifth.id);
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        message_count
    );
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let dry = engine
        .prune_context_checkpoints(&session.id, Some(0), Some(0), true)
        .await
        .unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.removed, 0);
    assert_eq!(
        engine
            .list_context_checkpoints(&session.id, Some(20))
            .await
            .unwrap()
            .checkpoints
            .len(),
        1
    );
}

#[tokio::test]
async fn manual_full_history_checkpoint_restores_every_message_without_a_summary() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "full history".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 3, "before the marker").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, Some("full history".into()))
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 4, "after the marker").await;
    let total = engine.list_messages(&session.id).await.unwrap().len();
    assert_eq!(total, 7);

    service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap();
    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert_eq!(loaded.messages.len(), 7);
    assert!(
        !loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
    assert!(loaded.messages[0].content.contains("before the marker"));
    assert!(loaded.messages[6].content.contains("after the marker"));
    let latest = engine
        .latest_event_of_kind(&session.id, "context.compacted")
        .await
        .unwrap()
        .expect("restore compaction");
    assert_eq!(latest.payload["mode"], "trim");
    assert!(latest.payload["through_message_id"].is_null());
}

#[tokio::test]
async fn trim_compaction_creates_a_summaryless_checkpoint_that_restores() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![Err(ProviderError::Stream(
            "summarizer unavailable".into(),
        ))]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id, "trim boundary".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 12, &"y".repeat(400)).await;
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);
    assert_eq!(
        result
            .compaction
            .as_ref()
            .map(|summary| summary.mode.as_str()),
        Some("trim")
    );
    let checkpoint = first_context_checkpoint(&engine, &session.id).await;
    assert_eq!(checkpoint.mode, "trim");
    assert!(
        checkpoint.summary.is_some(),
        "trim carries the authoritative state block"
    );
    assert!(checkpoint.through_message_id.is_some());
    set_session_idle(&engine, &session.id).await;
    let restored = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap();
    assert_eq!(restored.mode, "trim");
    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert!(
        !loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
    assert!(loaded.messages.len() < 12);
}

#[tokio::test]
async fn restore_refuses_a_checkpoint_whose_through_message_is_missing() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "missing through".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 3, "missing through content").await;
    set_session_idle(&engine, &session.id).await;
    let mut checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    checkpoint.id = "ctx-missing-through".into();
    checkpoint.summary = Some("Earlier conversation summary\n\nstate".into());
    checkpoint.mode = "summary".into();
    checkpoint.through_message_id = Some("message-that-does-not-exist".into());
    checkpoint.created_at = vp_protocol::now_rfc3339();
    engine
        .record_context_checkpoint(checkpoint.clone())
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("through message is missing"),
        "{error}"
    );
}

#[tokio::test]
async fn context_checkpoint_list_returns_the_most_recent_rows_first() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "list order".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "list order content").await;
    set_session_idle(&engine, &session.id).await;
    let first = service
        .context_checkpoint_now(&session.id, Some("one".into()))
        .await
        .unwrap();
    let second = service
        .context_checkpoint_now(&session.id, Some("two".into()))
        .await
        .unwrap();
    let third = service
        .context_checkpoint_now(&session.id, Some("three".into()))
        .await
        .unwrap();
    let limited = engine
        .list_context_checkpoints(&session.id, Some(2))
        .await
        .unwrap();
    assert_eq!(limited.limit, 2);
    assert_eq!(limited.checkpoints.len(), 2);
    assert_eq!(limited.checkpoints[0].id, second.id);
    assert_eq!(limited.checkpoints[1].id, third.id);
    assert_eq!(limited.latest_id.as_deref(), Some(third.id.as_str()));
    let all = engine
        .list_context_checkpoints(&session.id, Some(50))
        .await
        .unwrap();
    assert_eq!(all.checkpoints.len(), 3);
    assert_eq!(all.checkpoints[0].id, first.id);
    assert_eq!(all.checkpoints[2].id, third.id);
}

#[tokio::test]
async fn automatic_checkpoint_tracks_plan_revision_and_authoritative_state() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("planned summary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id, "plan checkpoint".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, "planned content").await;
    service.compact_now(&session.id).await.unwrap();
    let checkpoint = first_context_checkpoint(&engine, &session.id).await;
    assert_eq!(checkpoint.plan_revision, 1);
    let state = checkpoint.structured_state.as_ref().unwrap();
    assert_eq!(state.plan_revision, 1);
    assert_eq!(state.goal, "deferred continuation");
    assert_eq!(state.active_task_title.as_deref(), Some("Foundation"));
    assert_eq!(state.active_task_write_scope, vec!["src/one".to_string()]);
    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert_eq!(detail.preflight.plan_revision, 1);
    assert_eq!(detail.preflight.checkpoint_plan_revision, 1);
    assert_eq!(detail.preflight.provider, "queue");
    assert_eq!(detail.preflight.model, "test");
    assert!(detail.preflight.worktree_head == checkpoint.worktree_head);
}

#[tokio::test]
async fn restore_preflight_reports_missing_worktree_head_without_panicking() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "no head".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "no head content").await;
    set_session_idle(&engine, &session.id).await;
    let mut checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    checkpoint.id = "ctx-no-head".into();
    checkpoint.worktree_head.clear();
    checkpoint.created_at = vp_protocol::now_rfc3339();
    engine
        .record_context_checkpoint(checkpoint.clone())
        .await
        .unwrap();
    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(!detail.preflight.restorable);
    assert!(
        detail
            .preflight
            .errors
            .iter()
            .any(|error| error.contains("does not record a worktree head"))
    );
}

#[tokio::test]
async fn context_checkpoint_sweep_honors_the_project_policy_and_skips_imported_archives() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "sweep".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "sweep content").await;
    set_session_idle(&engine, &session.id).await;
    for index in 0..3 {
        service
            .context_checkpoint_now(&session.id, Some(format!("sweep {index}")))
            .await
            .unwrap();
    }
    engine
        .set_context_checkpoint_policy(
            &project.id,
            &vp_protocol::types::ContextCheckpointPolicy {
                enabled: true,
                max_checkpoints: 1,
                max_age_days: 0,
            },
        )
        .await
        .unwrap();
    let removed = engine.sweep_context_checkpoint_retention().await.unwrap();
    assert_eq!(removed, 2);
    let remaining = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(remaining.checkpoints.len(), 1);
    assert_eq!(remaining.checkpoints[0].label, "sweep 2");

    // An imported archive may keep rows, but the sweep must skip it rather
    // than failing the whole project pass.
    let imported = engine
        .create_session(project.id.clone(), "imported".into(), None)
        .await
        .unwrap();
    let mut checkpoint = remaining.checkpoints[0].clone();
    checkpoint.id = "ctx-imported-row".into();
    checkpoint.session_id = imported.id.clone();
    checkpoint.label = "imported".into();
    checkpoint.created_at = vp_protocol::now_rfc3339();
    engine.record_context_checkpoint(checkpoint).await.unwrap();
    engine
        .update_session_status(&imported.id, "imported")
        .await
        .unwrap();
    let removed = engine.sweep_context_checkpoint_retention().await.unwrap();
    assert_eq!(removed, 0);
    assert!(
        engine
            .list_context_checkpoints(&imported.id, Some(10))
            .await
            .unwrap()
            .checkpoints
            .iter()
            .any(|row| row.id == "ctx-imported-row")
    );
}

#[tokio::test]
async fn context_checkpoint_creation_and_restore_refuse_active_or_imported_sessions() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "active guard".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "active guard content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let error = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("idle, failed, or cancelled"),
        "{error}"
    );
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("idle, failed, or cancelled"),
        "{error}"
    );
    engine
        .update_session_status(&session.id, "imported")
        .await
        .unwrap();
    let error = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only archives"), "{error}");
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only archives"), "{error}");
}

#[tokio::test]
async fn force_never_bypasses_a_dirty_worktree_or_cost_budget() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "force safety".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 3, "force safety content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    let worktree = PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .unwrap(),
    );
    std::fs::write(worktree.join("force-dirty.txt"), "dirty\n").unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted changes"), "{error}");
    std::fs::remove_file(worktree.join("force-dirty.txt")).unwrap();

    let budget_session = engine
        .create_session_with_limits(project.id, "cost force".into(), None, Some(0.01), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&budget_session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &budget_session.id, 2, "cost force content").await;
    set_session_idle(&engine, &budget_session.id).await;
    let cost_checkpoint = service
        .context_checkpoint_now(&budget_session.id, None)
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &budget_session.id,
            "leader",
            "Leader",
            "leader",
            None,
            None,
            "test",
            1,
            1,
            false,
            0.02,
        )
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&budget_session.id, &cost_checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("budget is exhausted"), "{error}");
}

#[tokio::test]
async fn restore_events_record_forced_restore_and_keep_checkpoint_rows() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "forced restore".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 8, &"z".repeat(1_500)).await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    set_context_window(&service, 32_000, 1, 4, 1_000_000);
    let result = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap();
    assert!(result.warning.is_some());
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let restored = events
        .iter()
        .find(|event| event.kind == "context.restored")
        .expect("context.restored");
    assert_eq!(restored.payload["checkpoint_id"], checkpoint.id);
    assert_eq!(restored.payload["forced"], true);
    assert_eq!(restored.payload["warning"], result.warning.clone().unwrap());
    assert_eq!(
        restored.payload["compaction_seq"],
        result.compaction_event_seq
    );
    assert_eq!(
        restored.payload["checkpoint"]["id"].as_str(),
        Some(checkpoint.id.as_str())
    );
    assert_eq!(
        restored.payload["checkpoint"]["mode"].as_str(),
        Some(checkpoint.mode.as_str())
    );
    let still_listed = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(still_listed.checkpoints.len(), 1);
    assert_eq!(still_listed.checkpoints[0].id, checkpoint.id);
}

#[tokio::test]
async fn deleting_the_latest_idle_checkpoint_updates_status_without_touching_messages() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "delete latest".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "delete latest content").await;
    set_session_idle(&engine, &session.id).await;
    let first = service
        .context_checkpoint_now(&session.id, Some("first".into()))
        .await
        .unwrap();
    let second = service
        .context_checkpoint_now(&session.id, Some("second".into()))
        .await
        .unwrap();
    let messages = engine.list_messages(&session.id).await.unwrap().len();
    let deleted = engine
        .delete_context_checkpoint(&session.id, &second.id)
        .await
        .unwrap();
    assert!(deleted.removed);
    let status = engine.session_context_status(&session.id).await.unwrap();
    assert_eq!(status.checkpoint_count, 1);
    assert_eq!(
        status.latest_checkpoint_id.as_deref(),
        Some(first.id.as_str())
    );
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        messages
    );
    assert!(
        !engine
            .list_context_checkpoints(&session.id, Some(10))
            .await
            .unwrap()
            .checkpoints
            .iter()
            .any(|row| row.id == second.id)
    );
}

#[tokio::test]
async fn context_checkpoint_missing_ids_and_deleted_checkpoints_fail_closed() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "missing ids".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "missing ids content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    engine
        .delete_context_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
    let error = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
    let error = engine
        .list_context_checkpoints("missing-session", Some(10))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session not found"), "{error}");
    let error = service
        .context_checkpoint_now("missing-session", None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session not found"), "{error}");
    let error = engine
        .prune_context_checkpoints("missing-session", Some(0), Some(1), false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session not found"), "{error}");
}

#[tokio::test]
async fn restore_requires_a_configured_provider_but_detail_still_reports_the_boundary() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "unconfigured".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "unconfigured content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    let unconfigured = AgentService::unconfigured(engine.clone());
    let error = unconfigured
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("provider is not configured"),
        "{error}"
    );
    let detail = unconfigured
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert_eq!(detail.checkpoint.id, checkpoint.id);
    assert!(!detail.preflight.restorable);
    assert!(
        detail
            .preflight
            .errors
            .iter()
            .any(|error| error.contains("provider is not configured"))
    );
}

#[tokio::test]
async fn restore_refuses_a_missing_worktree_and_detail_stays_diagnostic() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "missing worktree".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "missing worktree content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    let worktree = PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .unwrap(),
    );
    let moved = worktree.with_extension("moved");
    std::fs::rename(&worktree, &moved).unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("worktree is missing"), "{error}");
    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(
        detail
            .preflight
            .errors
            .iter()
            .any(|error| error.contains("worktree is missing"))
    );
    std::fs::rename(&moved, &worktree).unwrap();
    service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn prune_and_delete_reject_imported_archives_and_invalid_retention() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "imported pruning".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "imported")
        .await
        .unwrap();
    let error = engine
        .prune_context_checkpoints(&session.id, Some(0), Some(1), false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only archives"), "{error}");
    let error = engine
        .delete_context_checkpoint(&session.id, "missing")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only archives"), "{error}");
    let error = engine
        .prune_context_checkpoints(&session.id, Some(366), Some(1), false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("at most 365"), "{error}");
}

#[tokio::test]
async fn checkpoint_labels_are_normalized_and_defaulted_by_the_daemon_path() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "labels".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "labels content").await;
    set_session_idle(&engine, &session.id).await;
    let normalized = service
        .context_checkpoint_now(&session.id, Some("  before    the\nrefactor  ".into()))
        .await
        .unwrap();
    assert_eq!(normalized.label, "before the refactor");
    let fallback = service
        .context_checkpoint_now(&session.id, Some("   ".into()))
        .await
        .unwrap();
    assert!(fallback.label.starts_with("context checkpoint "));
    assert!(fallback.label.len() <= 120);
}

#[tokio::test]
async fn a_running_agent_compaction_creates_an_automatic_checkpoint_and_restores() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("automatic run summary".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("compaction complete".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 3, 1_000_000);
    let session = engine
        .create_session(project.id, "automatic run checkpoint".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..20 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!(
                    "message {index}: the automatic checkpoint investigation includes enough detail to exceed the tiny threshold"
                ),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    service
        .start(&session.id, "finish after automatic compaction")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");

    let checkpoints = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(checkpoints.checkpoints.len(), 1);
    let checkpoint = checkpoints.checkpoints[0].clone();
    assert!(checkpoint.automatic);
    assert_eq!(checkpoint.mode, "summary");
    assert!(checkpoint.summary.is_some());
    assert!(checkpoint.through_message_id.is_some());
    assert!(checkpoint.source_compaction_seq.is_some());
    let compacted = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.compacted")
        .expect("context.compacted");
    assert_eq!(
        compacted.payload["context_checkpoint_id"].as_str(),
        Some(checkpoint.id.as_str())
    );
    assert_eq!(
        compacted.payload["summary"].as_str(),
        checkpoint.summary.as_deref()
    );

    let messages = engine.list_messages(&session.id).await.unwrap().len();
    let restored = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap();
    assert_eq!(restored.checkpoint_id, checkpoint.id);
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        messages
    );
    let loaded = load_conversation(&engine, &session.id).await.unwrap();
    assert!(
        loaded.messages[0]
            .content
            .starts_with("Earlier conversation summary")
    );
}

#[tokio::test]
async fn restore_refuses_a_non_descendant_worktree_head() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("non descendant boundary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id, "non descendant".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, "non descendant content").await;
    service.compact_now(&session.id).await.unwrap();
    let checkpoint = first_context_checkpoint(&engine, &session.id).await;
    set_session_idle(&engine, &session.id).await;
    let worktree = PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .unwrap(),
    );
    let tree = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["rev-parse", "HEAD^{tree}"])
        .output()
        .unwrap();
    assert!(tree.status.success());
    let tree = String::from_utf8(tree.stdout).unwrap().trim().to_string();
    let orphan = Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["commit-tree", &tree, "-m", "unrelated root"])
        .output()
        .unwrap();
    assert!(orphan.status.success());
    let orphan = String::from_utf8(orphan.stdout).unwrap().trim().to_string();
    let mut unrelated = checkpoint.clone();
    unrelated.id = "ctx-non-descendant".into();
    unrelated.worktree_head = orphan;
    unrelated.created_at = vp_protocol::now_rfc3339();
    engine
        .record_context_checkpoint(unrelated.clone())
        .await
        .unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &unrelated.id, true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a descendant"), "{error}");
    let detail = service
        .context_checkpoint_detail(&session.id, &unrelated.id)
        .await
        .unwrap();
    assert!(!detail.preflight.worktree_descendant);
    assert!(
        detail
            .preflight
            .errors
            .iter()
            .any(|error| error.contains("not a descendant"))
    );
}

#[tokio::test]
async fn restore_preflight_aggregates_plan_model_and_worktree_errors() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "aggregate errors".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 3, "aggregate errors content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.model = "other-model".into();
        *service.config.write().unwrap() = Some(config);
    }
    let worktree = PathBuf::from(
        engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .worktree
            .unwrap(),
    );
    std::fs::write(worktree.join("aggregate-dirty.txt"), "dirty\n").unwrap();
    let error = service
        .restore_context_checkpoint_now(&session.id, &checkpoint.id, true)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("plan revision changed"), "{message}");
    assert!(message.contains("model changed"), "{message}");
    assert!(message.contains("uncommitted changes"), "{message}");
}

#[tokio::test]
async fn manual_checkpoint_after_compaction_preserves_state_and_source_sequence() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("manual state summary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id, "manual state".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &continuation_test_plan())
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, "manual state content").await;
    service.compact_now(&session.id).await.unwrap();
    set_session_idle(&engine, &session.id).await;
    let auto = first_context_checkpoint(&engine, &session.id).await;
    let manual = service
        .context_checkpoint_now(&session.id, Some("manual copy".into()))
        .await
        .unwrap();
    assert_eq!(manual.mode, auto.mode);
    assert_eq!(manual.summary, auto.summary);
    assert_eq!(manual.through_message_id, auto.through_message_id);
    assert_eq!(manual.plan_revision, auto.plan_revision);
    assert_eq!(manual.structured_state, auto.structured_state);
    assert_eq!(manual.source_compaction_seq, auto.source_compaction_seq);
    assert_eq!(manual.tokens_before, auto.tokens_before);
    assert_eq!(manual.tokens_after, auto.tokens_after);
    assert!(!manual.automatic);
    let detail = service
        .context_checkpoint_detail(&session.id, &manual.id)
        .await
        .unwrap();
    assert!(
        !detail.is_active,
        "the auto checkpoint is the active reference"
    );
    assert!(
        detail
            .summary_preview
            .contains("Earlier conversation summary")
    );
}

#[tokio::test]
async fn a_newer_compaction_makes_the_older_checkpoint_inactive_but_restorable() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("first inactive boundary".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("second active boundary".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 2, 1_000_000);
    let session = engine
        .create_session(project.id, "inactive boundary".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 10, "first inactive content").await;
    service.compact_now(&session.id).await.unwrap();
    let first = first_context_checkpoint(&engine, &session.id).await;
    append_context_messages(&engine, &session.id, 10, "second active content").await;
    service.compact_now(&session.id).await.unwrap();
    let checkpoints = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    let second = checkpoints.checkpoints.last().unwrap().clone();
    assert_ne!(first.id, second.id);
    set_session_idle(&engine, &session.id).await;
    let first_detail = service
        .context_checkpoint_detail(&session.id, &first.id)
        .await
        .unwrap();
    assert!(!first_detail.is_active);
    let second_detail = service
        .context_checkpoint_detail(&session.id, &second.id)
        .await
        .unwrap();
    assert!(second_detail.is_active);
    let restored = service
        .restore_context_checkpoint_now(&session.id, &first.id, true)
        .await
        .unwrap();
    assert_eq!(restored.checkpoint_id, first.id);
    let after = service
        .context_checkpoint_detail(&session.id, &first.id)
        .await
        .unwrap();
    assert!(after.is_active);
    let second_after = service
        .context_checkpoint_detail(&session.id, &second.id)
        .await
        .unwrap();
    assert!(!second_after.is_active);
    assert_eq!(
        engine
            .list_context_checkpoints(&session.id, Some(10))
            .await
            .unwrap()
            .checkpoints
            .len(),
        2,
        "restore never deletes checkpoint rows"
    );
}

#[tokio::test]
async fn automatic_checkpoint_message_cursor_matches_the_stored_boundary() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("cursor summary".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_context_window(&service, 32_000, 1_000, 3, 1_000_000);
    let session = engine
        .create_session(project.id, "cursor boundary".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 12, "cursor boundary content").await;
    service.compact_now(&session.id).await.unwrap();
    let checkpoint = first_context_checkpoint(&engine, &session.id).await;
    let messages = engine.list_messages(&session.id).await.unwrap();
    let position = messages
        .iter()
        .position(|message| Some(&message.id) == checkpoint.through_message_id.as_ref())
        .expect("through message");
    assert_eq!(checkpoint.message_cursor, position + 1);
}

#[tokio::test]
async fn prune_uses_the_project_policy_defaults_when_arguments_are_omitted() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "policy prune".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "policy prune content").await;
    set_session_idle(&engine, &session.id).await;
    for index in 0..4 {
        service
            .context_checkpoint_now(&session.id, Some(format!("policy {index}")))
            .await
            .unwrap();
    }
    engine
        .set_context_checkpoint_policy(
            &project.id,
            &vp_protocol::types::ContextCheckpointPolicy {
                enabled: false,
                max_checkpoints: 2,
                max_age_days: 0,
            },
        )
        .await
        .unwrap();
    let result = engine
        .prune_context_checkpoints(&session.id, None, None, false)
        .await
        .unwrap();
    assert_eq!(result.candidates, 2);
    assert_eq!(result.removed, 2);
    let rows = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(rows.checkpoints.len(), 2);
    assert_eq!(rows.checkpoints[0].label, "policy 2");
    assert_eq!(rows.checkpoints[1].label, "policy 3");
}

#[tokio::test]
async fn fresh_sessions_report_zero_context_checkpoints() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "fresh status".into(), None)
        .await
        .unwrap();
    let status = engine.session_context_status(&session.id).await.unwrap();
    assert_eq!(status.checkpoint_count, 0);
    assert!(status.latest_checkpoint_id.is_none());
    assert!(status.latest_checkpoint_at.is_none());
    let listed = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert!(listed.checkpoints.is_empty());
    assert!(listed.latest_id.is_none());
    assert_eq!(listed.limit, 10);
}

#[tokio::test]
async fn manual_checkpoint_works_without_an_explicit_leader_agent_row() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "fallback leader".into(), None)
        .await
        .unwrap();
    append_context_messages(&engine, &session.id, 2, "fallback leader content").await;
    set_session_idle(&engine, &session.id).await;
    let checkpoint = service
        .context_checkpoint_now(&session.id, Some("fallback".into()))
        .await
        .unwrap();
    assert_eq!(checkpoint.provider, "queue");
    assert_eq!(checkpoint.model, "test");
    assert!(!checkpoint.worktree_head.is_empty());
    let detail = service
        .context_checkpoint_detail(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(detail.preflight.restorable, "{:?}", detail.preflight.errors);
}

#[tokio::test]
async fn deleting_a_missing_context_checkpoint_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "missing delete".into(), None)
        .await
        .unwrap();
    let result = engine
        .delete_context_checkpoint(&session.id, "ctx-does-not-exist")
        .await
        .unwrap();
    assert!(!result.removed);
    assert_eq!(result.checkpoint_id, "ctx-does-not-exist");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "context.checkpoint.deleted")
    );
}

#[tokio::test]
async fn context_checkpoint_list_limit_is_clamped_to_the_protocol_ceiling() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "limit clamp".into(), None)
        .await
        .unwrap();
    let listed = engine
        .list_context_checkpoints(&session.id, Some(10_000))
        .await
        .unwrap();
    assert_eq!(listed.limit, 200);
}

#[test]
fn citation_registry_only_keys_returned_chunks_and_deduplicates() {
    let mut registry = CitationRegistry::default();
    let first = SourceSearchHit {
        source_id: "src-1".into(),
        source_title: "Runtime".into(),
        chunk_id: "src-1:0001:aaa".into(),
        chunk_no: 1,
        content: "Deterministic leases.".into(),
        score: 0.8,
        source: "hybrid".into(),
        confidence: 0.8,
        origins: vec!["lexical".into(), "semantic".into()],
        lexical_score: Some(1.2),
        semantic_score: Some(0.7),
        ..SourceSearchHit::default()
    };
    let same_chunk = SourceSearchHit {
        score: 0.9,
        confidence: 0.9,
        ..first.clone()
    };
    let second = SourceSearchHit {
        chunk_id: "src-1:0002:bbb".into(),
        chunk_no: 2,
        content: "Integration evidence.".into(),
        score: 0.6,
        ..first.clone()
    };
    assert_eq!(registry.add_hit(&first, "search"), "c1");
    assert_eq!(
        registry.add_hit(&same_chunk, "search"),
        "c1",
        "a chunk keeps one key across lanes"
    );
    assert_eq!(registry.add_hit(&second, "read"), "c2");
    let citations = registry.list();
    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0].key, "c1");
    assert_eq!(citations[0].origin, "search");
    assert_eq!(citations[0].score, 0.8, "first citation snapshot is stable");
    assert_eq!(citations[1].key, "c2");
    assert_eq!(citations[1].origin, "read");
    // There is no API that mints a key for a chunk that was not returned; the
    // only keys in the list are the two registered hits.
    assert!(citations.iter().all(
        |citation| citation.chunk_id == first.chunk_id || citation.chunk_id == second.chunk_id
    ));
    assert!(
        citations
            .iter()
            .all(|citation| citation.key == "c1" || citation.key == "c2")
    );
}

#[test]
fn session_budget_reserves_worker_and_labour_through_scaling_slots() {
    let slots = Arc::new(crate::scaling::ScalingSlots::new(2, 1));
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 2,
            labour: 1,
        },
    )
    .with_scaling_slots(slots.clone());
    let (_, first) = budget.reserve_slot(AgentRole::Worker).expect("worker 1");
    let (_, second) = budget.reserve_slot(AgentRole::Worker).expect("worker 2");
    assert!(budget.reserve_slot(AgentRole::Worker).is_none());
    let (_, labour) = budget.reserve_slot(AgentRole::Labour).expect("labour 1");
    assert!(budget.reserve_slot(AgentRole::Labour).is_none());
    assert_eq!(slots.active(), (2, 1));
    assert_eq!(budget.available(AgentRole::Worker), 0);

    slots.apply(3, 2);
    assert_eq!(budget.worker_cap(), 3);
    assert_eq!(budget.available(AgentRole::Worker), 1);
    assert_eq!(budget.available(AgentRole::Labour), 1);
    let (_, third) = budget.reserve_slot(AgentRole::Worker).expect("worker 3");
    assert!(budget.reserve_slot(AgentRole::Worker).is_none());

    drop(first);
    drop(second);
    drop(third);
    drop(labour);
    assert_eq!(slots.active(), (0, 0));
}

#[test]
fn session_budget_legacy_expansion_yields_to_managed_scaling() {
    let slots = Arc::new(crate::scaling::ScalingSlots::new(2, 2));
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 2,
            labour: 2,
        },
    )
    .with_scaling_slots(slots.clone());
    budget.expand_workers(4);
    assert_eq!(budget.worker_cap(), 4);
    assert_eq!(slots.targets().0, 4);

    slots.enable_managed();
    budget.expand_workers(8);
    assert_eq!(budget.worker_cap(), 4);
    assert_eq!(slots.targets().0, 4);
    budget.set_worker_cap(1);
    assert_eq!(budget.worker_cap(), 4);
    assert_eq!(slots.targets().0, 4);
}

#[test]
fn governor_rejection_is_task_scoped_and_non_abortive() {
    let rejection = Rejection {
        scope: "department",
        id: "impl".to_string(),
        reason: crate::budget::BudgetReason::Reserved,
        limit_tokens: Some(10_000),
        spent_tokens: 2_000,
        reserved_tokens: 100,
        limit_cost: None,
        spent_cost: 0.0,
        reserved_cost: 0.0,
        protected_tokens: 6_975,
    };
    let stop = BudgetStop::from_governor_rejection(rejection, Some("T1"));
    assert_eq!(stop.scope, "task");
    assert_eq!(stop.id.as_deref(), Some("T1"));
    assert_eq!(stop.code, "BUDGET_RESERVED");
    assert!(!stop.is_abortive());
    assert!(stop.summary().contains("critical-path budget reserve"));
    assert_eq!(stop.payload()["protected_tokens"], 6_975);
}

#[test]
fn governor_throttle_stop_is_task_scoped_and_retryable() {
    let rejection = Rejection {
        scope: "task",
        id: "T2".to_string(),
        reason: crate::budget::BudgetReason::Throttled,
        limit_tokens: Some(10_000),
        spent_tokens: 8_000,
        reserved_tokens: 0,
        limit_cost: None,
        spent_cost: 0.0,
        reserved_cost: 0.0,
        protected_tokens: 1_500,
    };
    let stop = BudgetStop::from_governor_rejection(rejection, Some("T2"));
    assert_eq!(stop.code, "BUDGET_THROTTLED");
    assert!(!stop.is_abortive());
    assert!(stop.summary().contains("budget governor throttle"));
}

fn test_remote_host() -> vp_protocol::remote::ExecutionHost {
    vp_protocol::remote::ExecutionHost {
        mode: vp_protocol::remote::ExecutionHostMode::Ssh,
        host: "build.example.com".into(),
        user: Some("agent".into()),
        root: "/srv/repo".into(),
        token_env: Some("VERLINPARK_TEST_REMOTE_TOKEN".into()),
        ..vp_protocol::remote::ExecutionHost::default()
    }
}

fn test_remote_policy() -> vp_protocol::remote::RemoteIntegrationPolicy {
    vp_protocol::remote::RemoteIntegrationPolicy {
        enabled: true,
        max_bundle_bytes: 8 * 1024 * 1024,
        require_verifier: true,
        allowed_hosts: vec!["agent@build.example.com".into()],
        keep_import_refs: false,
    }
}

fn git_test(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn remote_bundle_fixture(
    project_root: &Path,
    relative_path: &str,
    content: &str,
) -> (
    TempDir,
    PathBuf,
    vp_protocol::remote::RemoteBundleSummary,
    String,
) {
    let remote = TempDir::new().unwrap();
    let clone = remote.path().join("remote");
    let status = Command::new("git")
        .args(["clone", "-q"])
        .arg(project_root)
        .arg(&clone)
        .status()
        .unwrap();
    assert!(status.success());
    git_test(&clone, &["config", "user.email", "test@example.com"]);
    git_test(&clone, &["config", "user.name", "Remote Test"]);
    git_test(&clone, &["checkout", "-q", "-b", "feature/remote"]);
    let target = clone.join(relative_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&target, content).unwrap();
    git_test(&clone, &["add", relative_path]);
    git_test(&clone, &["commit", "-q", "-m", "remote change"]);
    let head = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&clone)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let bundle_dir = TempDir::new().unwrap();
    let bundle_path = bundle_dir.path().join("remote.bundle");
    git_test(
        &clone,
        &[
            "bundle",
            "create",
            bundle_path.to_str().unwrap(),
            "feature/remote",
        ],
    );
    let bytes = std::fs::metadata(&bundle_path).unwrap().len();
    let sha256 = crate::remote_integration::sha256_file(&bundle_path).unwrap();
    let summary = vp_protocol::remote::RemoteBundleSummary {
        host: "build.example.com".into(),
        branch: "feature/remote".into(),
        head: head.clone(),
        sha256,
        bytes,
        chunks: 1,
        chunk_bytes: 1024 * 1024,
        imported_at: vp_protocol::now_rfc3339(),
    };
    // Keep both the clone and the bundle alive: the bundle lives in its own
    // temp dir, while the clone is the source repository for the branch.
    (bundle_dir, bundle_path, summary, head)
}

async fn configure_remote_integration(
    engine: &Engine,
    project_id: &str,
    policy: vp_protocol::remote::RemoteIntegrationPolicy,
) {
    engine
        .set_execution_host(project_id, test_remote_host())
        .await
        .unwrap();
    engine
        .set_remote_integration_policy(project_id, policy, true)
        .await
        .unwrap();
}

fn remote_params(
    project_id: &str,
    session_id: &str,
) -> vp_protocol::remote::RemoteIntegrationParams {
    vp_protocol::remote::RemoteIntegrationParams {
        project_id: project_id.to_string(),
        session_id: session_id.to_string(),
        branch: "feature/remote".into(),
        host: None,
        expected_commit: None,
        base_commit: None,
        task_id: None,
        objective: Some("Integrate the remote change".into()),
        confirm: true,
        keep_import_refs: None,
    }
}

fn verdict_provider(verdicts: usize) -> Arc<QueueProvider> {
    Arc::new(QueueProvider {
        responses: Mutex::new(
            (0..verdicts)
                .map(|index| {
                    vec![
                        StreamEvent::ToolCall(ToolCall {
                            id: format!("verdict-{index}"),
                            name: "submit_verdict".into(),
                            arguments:
                                r#"{"approved":true,"summary":"remote import looks correct"}"#
                                    .into(),
                        }),
                        StreamEvent::Done,
                    ]
                })
                .collect(),
        ),
    })
}

#[tokio::test]
async fn remote_integration_fails_before_connecting_when_policy_denies_it() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "remote policy".into(), None)
        .await
        .unwrap();
    let (_bundle_dir, bundle_path, bundle, _head) =
        remote_bundle_fixture(Path::new(project.root.as_str()), "remote.txt", "remote\n");
    let error = service
        .integrate_remote(remote_params(&project.id, &session.id))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_INTEGRATION_DISABLED"));

    // Enabling without confirmation is rejected before the policy is stored.
    engine
        .set_execution_host(&project.id, test_remote_host())
        .await
        .unwrap();
    let error = engine
        .set_remote_integration_policy(&project.id, test_remote_policy(), false)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_INTEGRATION_APPROVAL_REQUIRED"));
    assert!(
        !engine
            .get_remote_integration_policy(&project.id)
            .await
            .unwrap()
            .enabled
    );

    engine
        .set_remote_integration_policy(&project.id, test_remote_policy(), true)
        .await
        .unwrap();
    let mut wrong_host = remote_params(&project.id, &session.id);
    wrong_host.host = Some("other.example.com".into());
    let error = service.integrate_remote(wrong_host).await.unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_INTEGRATION_HOST_NOT_ALLOWED"));

    let mut unconfirmed = remote_params(&project.id, &session.id);
    unconfirmed.confirm = false;
    let error = service.integrate_remote(unconfirmed).await.unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_INTEGRATION_APPROVAL_REQUIRED"));

    let mut disabled = test_remote_policy();
    disabled.enabled = false;
    engine
        .set_remote_integration_policy(&project.id, disabled, true)
        .await
        .unwrap();
    let error = service
        .integrate_remote_bundle_file(remote_params(&project.id, &session.id), bundle_path, bundle)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_INTEGRATION_DISABLED"));
    assert!(
        engine
            .list_merge_queue(&session.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn imports_verifies_and_integrates_a_remote_bundle() {
    let dir = TempDir::new().unwrap();
    let provider = verdict_provider(1);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    configure_remote_integration(&engine, &project.id, test_remote_policy()).await;
    let session = engine
        .create_session(project.id.clone(), "remote import".into(), None)
        .await
        .unwrap();
    let (_bundle_dir, bundle_path, bundle, head) = remote_bundle_fixture(
        Path::new(project.root.as_str()),
        "remote.txt",
        "from remote\n",
    );
    let result = service
        .integrate_remote_bundle_file(remote_params(&project.id, &session.id), bundle_path, bundle)
        .await
        .unwrap();
    assert_eq!(result.entry.status, "merged");
    assert_eq!(result.entry.source, "remote");
    assert_eq!(
        result.entry.remote_host.as_deref(),
        Some("build.example.com")
    );
    assert_eq!(
        result.entry.remote_branch.as_deref(),
        Some("feature/remote")
    );
    assert_eq!(result.entry.commit, head);
    let effects = engine
        .session_effects(&session.id, None, true, 100)
        .await
        .unwrap();
    assert!(effects.receipts.iter().any(|receipt| {
        receipt.target.kind == "remote_integration"
            && receipt.target.key == "build.example.com:feature/remote"
            && receipt.state == EffectState::Done
            && receipt.external_id.as_deref() == Some(head.as_str())
    }));
    assert_eq!(result.entry.verifier_approved, Some(true));
    assert_eq!(
        result.entry.verifier_summary.as_deref(),
        Some("remote import looks correct")
    );
    assert!(result.entry.environment_caveat.is_some());
    assert!(result.verification.required);
    assert!(result.verification.performed);
    assert_eq!(result.verification.approved, Some(true));
    assert!(result.refs_pruned);
    assert!(result.imported_ref.is_none());
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("remote.txt")).unwrap(),
        "from remote\n"
    );
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].source, "remote");
    assert_eq!(queue[0].status, "merged");
    assert_eq!(queue[0].bundle_sha256, Some(result.bundle.sha256.clone()));
    assert_eq!(queue[0].verifier_approved, Some(true));
    assert!(queue[0].environment_caveat.is_some());
    assert!(!crate::remote_integration::import_ref_exists(
        Path::new(&project.root),
        queue[0].imported_ref.as_deref().unwrap()
    ));
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "remote.integration.approved")
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["source"] == "remote"
            && event.payload["bundle_sha256"] == result.bundle.sha256
            && event.payload["environment_caveat"].is_string()
    }));
    assert!(events.iter().any(|event| {
        event.kind == "merge_queue.enqueued"
            && event.payload["entry"]["source"] == "remote"
            && event.payload["entry"]["imported_ref"].is_string()
    }));
    assert!(
        events
            .iter()
            .any(|event| event.kind == "remote.integration.finished"
                && event.payload["status"] == "merged")
    );
}

#[tokio::test]
async fn optional_verifier_skips_the_provider_and_still_integrates() {
    let dir = TempDir::new().unwrap();
    let provider = verdict_provider(0);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut policy = test_remote_policy();
    policy.require_verifier = false;
    configure_remote_integration(&engine, &project.id, policy).await;
    let session = engine
        .create_session(project.id.clone(), "remote optional".into(), None)
        .await
        .unwrap();
    let (_bundle_dir, bundle_path, bundle, _head) = remote_bundle_fixture(
        Path::new(project.root.as_str()),
        "optional.txt",
        "optional\n",
    );
    let result = service
        .integrate_remote_bundle_file(remote_params(&project.id, &session.id), bundle_path, bundle)
        .await
        .unwrap();
    assert_eq!(result.entry.status, "merged");
    assert!(!result.verification.required);
    assert!(!result.verification.performed);
    assert_eq!(result.verification.approved, None);
    assert_eq!(result.entry.verifier_approved, None);
    assert!(result.entry.environment_caveat.is_some());
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| { event.kind == "remote.integration.verification_skipped" })
    );
}

#[tokio::test]
async fn conflict_retains_the_imported_ref_and_retry_reverifies_then_merges() {
    let dir = TempDir::new().unwrap();
    let provider = verdict_provider(2);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    configure_remote_integration(&engine, &project.id, test_remote_policy()).await;
    let session = engine
        .create_session(project.id.clone(), "remote conflict".into(), None)
        .await
        .unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(session_worktree.join("README.md"), "session side\n").unwrap();
    let session_commit = crate::worktree::commit_all(&session_worktree, "session side").unwrap();
    assert!(session_commit.is_some());
    let (_bundle_dir, bundle_path, bundle, _head) = remote_bundle_fixture(
        Path::new(project.root.as_str()),
        "README.md",
        "remote side\n",
    );
    let error = service
        .integrate_remote_bundle_file(remote_params(&project.id, &session.id), bundle_path, bundle)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_INTEGRATION_FAILED"));
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    let entry = &queue[0];
    assert_eq!(entry.status, "conflict");
    let imported_ref = entry.imported_ref.clone().unwrap();
    assert!(crate::remote_integration::import_ref_exists(
        Path::new(&project.root),
        &imported_ref
    ));

    // Resolve the rebase in the local remote worktree exactly as an operator
    // would, update the imported ref, then retry through the same Verifier.
    let agents = engine.list_agents(&session.id).await.unwrap();
    let agent = agents
        .iter()
        .find(|agent| agent.name.starts_with("Remote "))
        .unwrap();
    let remote_worktree = PathBuf::from(agent.worktree.clone().unwrap());
    let _ = crate::worktree::rebase_onto_keep_conflicts(
        &remote_worktree,
        &session.branch.clone().unwrap(),
    );
    std::fs::write(remote_worktree.join("README.md"), "resolved both sides\n").unwrap();
    crate::worktree::stage_all(&remote_worktree).unwrap();
    crate::worktree::continue_rebase(&remote_worktree).unwrap();
    let resolved_commit = crate::worktree::head_commit(&remote_worktree).unwrap();
    crate::remote_integration::update_import_ref(
        Path::new(&project.root),
        &imported_ref,
        &resolved_commit,
    )
    .unwrap();
    let retried = service.retry_merge(&entry.id).await.unwrap();
    assert_eq!(retried.status, "merged");
    assert_eq!(retried.source, "remote");
    assert_eq!(retried.attempt, 2);
    assert_eq!(
        std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
        "resolved both sides\n"
    );
    assert!(!crate::remote_integration::import_ref_exists(
        Path::new(&project.root),
        &imported_ref
    ));
}

#[tokio::test]
async fn tampered_and_head_mismatched_bundles_never_enter_the_queue() {
    let dir = TempDir::new().unwrap();
    let provider = verdict_provider(0);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    configure_remote_integration(&engine, &project.id, test_remote_policy()).await;
    let session = engine
        .create_session(project.id.clone(), "remote tamper".into(), None)
        .await
        .unwrap();
    let (_bundle_dir, bundle_path, mut bundle, _head) =
        remote_bundle_fixture(Path::new(project.root.as_str()), "tamper.txt", "tamper\n");
    bundle.sha256 = "0".repeat(64);
    let error = service
        .integrate_remote_bundle_file(
            remote_params(&project.id, &session.id),
            bundle_path.clone(),
            bundle.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_BUNDLE_TAMPERED"));

    let (_bundle_dir2, bundle_path2, mut bundle2, _head2) =
        remote_bundle_fixture(Path::new(project.root.as_str()), "tamper.txt", "tamper\n");
    bundle2.head = "1".repeat(40);
    let error = service
        .integrate_remote_bundle_file(
            remote_params(&project.id, &session.id),
            bundle_path2,
            bundle2,
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("REMOTE_BUNDLE_HEAD_MISMATCH"));
    assert!(
        engine
            .list_merge_queue(&session.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn remote_queue_provenance_replays_from_the_enqueue_event() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "remote replay".into(), None)
        .await
        .unwrap();
    let source = vp_protocol::types::MergeQueueSource::remote(
        "build.example.com",
        "feature/remote",
        "a".repeat(64),
        "refs/verlinpark/remote/session/import-1",
    );
    engine
        .enqueue_merge_with_source(
            &session.id,
            "remote-agent",
            "Remote Worker",
            None,
            "verlinpark/session--remote-import-1",
            "abcdef1234567890",
            "1234567890abcdef",
            Some("verified locally".to_string()),
            source,
        )
        .await
        .unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(report.queue_matches);
    let _ = engine.repair_session(&session.id).await.unwrap();
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].source, "remote");
    assert_eq!(queue[0].remote_host.as_deref(), Some("build.example.com"));
    assert_eq!(
        queue[0].imported_ref.as_deref(),
        Some("refs/verlinpark/remote/session/import-1")
    );
}

#[tokio::test]
async fn remote_ref_cleanup_protects_retryable_entries_unless_forced() {
    let dir = TempDir::new().unwrap();
    let provider = verdict_provider(1);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    configure_remote_integration(&engine, &project.id, test_remote_policy()).await;
    let session = engine
        .create_session(project.id.clone(), "remote cleanup".into(), None)
        .await
        .unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(session_worktree.join("README.md"), "session cleanup side\n").unwrap();
    crate::worktree::commit_all(&session_worktree, "session side").unwrap();
    let (_bundle_dir, bundle_path, bundle, _head) = remote_bundle_fixture(
        Path::new(project.root.as_str()),
        "README.md",
        "remote cleanup side\n",
    );
    let _ = service
        .integrate_remote_bundle_file(remote_params(&project.id, &session.id), bundle_path, bundle)
        .await
        .unwrap_err();
    let entry = engine.list_merge_queue(&session.id).await.unwrap()[0].clone();
    let imported_ref = entry.imported_ref.clone().unwrap();
    let agent = engine
        .list_agents(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.name.starts_with("Remote "))
        .unwrap();
    let worktree = PathBuf::from(agent.worktree.clone().unwrap());
    assert!(worktree.exists());
    let protected = engine
        .remote_integration_cleanup(vp_protocol::remote::RemoteIntegrationCleanupParams {
            project_id: project.id.clone(),
            session_id: Some(session.id.clone()),
            entry_id: None,
            force: false,
        })
        .await
        .unwrap();
    assert!(protected.removed_refs.is_empty());
    assert!(
        protected
            .skipped
            .iter()
            .any(|reason| reason.contains("requires force"))
    );
    assert!(crate::remote_integration::import_ref_exists(
        Path::new(&project.root),
        &imported_ref
    ));
    let forced = engine
        .remote_integration_cleanup(vp_protocol::remote::RemoteIntegrationCleanupParams {
            project_id: project.id.clone(),
            session_id: Some(session.id.clone()),
            entry_id: Some(entry.id.clone()),
            force: true,
        })
        .await
        .unwrap();
    assert_eq!(forced.removed_refs, vec![imported_ref.clone()]);
    assert!(
        forced
            .removed_worktrees
            .iter()
            .any(|path| path == &worktree.to_string_lossy())
    );
    assert!(!crate::remote_integration::import_ref_exists(
        Path::new(&project.root),
        &imported_ref
    ));
    assert!(!worktree.exists());
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "remote.integration.refs_pruned"
            && event.payload["entry_id"] == entry.id
            && event.payload["force"] == true
    }));
}

#[tokio::test]
async fn remote_integration_status_reports_counts_and_retained_refs() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "remote status".into(), None)
        .await
        .unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    let commit = crate::worktree::head_commit(&session_worktree).unwrap();
    let reference = "refs/verlinpark/remote/status/import-1".to_string();
    crate::remote_integration::update_import_ref(Path::new(&project.root), &reference, &commit)
        .unwrap();
    let source = vp_protocol::types::MergeQueueSource::remote(
        "build.example.com",
        "feature/status",
        "b".repeat(64),
        reference.clone(),
    );
    let merged = engine
        .enqueue_merge_with_source(
            &session.id,
            "remote-agent-1",
            "Remote One",
            None,
            "verlinpark/status--remote-1",
            &commit,
            &commit,
            Some("merged".into()),
            source.clone(),
        )
        .await
        .unwrap();
    engine
        .update_merge_queue_entry(&merged.id, "merged", Some("ok".into()))
        .await
        .unwrap();
    let conflict = engine
        .enqueue_merge_with_source(
            &session.id,
            "remote-agent-2",
            "Remote Two",
            None,
            "verlinpark/status--remote-2",
            &commit,
            &commit,
            Some("conflict".into()),
            vp_protocol::types::MergeQueueSource::remote(
                "build.example.com",
                "feature/conflict",
                "c".repeat(64),
                "refs/verlinpark/remote/status/missing",
            ),
        )
        .await
        .unwrap();
    engine
        .update_merge_queue_entry(&conflict.id, "conflict", Some("conflict".into()))
        .await
        .unwrap();
    engine
        .set_merge_queue_verification(
            &merged.id,
            Some(true),
            Some("approved by verifier".into()),
            Some("local only".into()),
        )
        .await
        .unwrap();
    let status = engine
        .remote_integration_status(&project.id, Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(status.total, 2);
    assert_eq!(status.merged, 1);
    assert_eq!(status.conflicts, 1);
    assert_eq!(status.pending, 0);
    assert_eq!(status.failed, 0);
    assert_eq!(status.retained_refs, 1);
    assert!(status.verifier_required);
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.entry_id == merged.id)
        .unwrap();
    assert_eq!(entry.verifier_approved, Some(true));
    assert_eq!(
        entry.verifier_summary.as_deref(),
        Some("approved by verifier")
    );
    assert_eq!(entry.environment_caveat.as_deref(), Some("local only"));
    let policy = engine
        .remote_integration_policy_result(&project.id)
        .await
        .unwrap();
    assert!(!policy.policy.enabled);
    assert_eq!(policy.project_id, project.id);
}

#[tokio::test]
async fn integration_command_cache_is_off_by_default_and_opt_in_when_enabled() {
    let data = TempDir::new().unwrap();
    let worktree = TempDir::new().unwrap();
    std::fs::write(worktree.path().join(".gitignore"), "runs.log\n").unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(worktree.path().to_str().unwrap(), None)
        .unwrap();
    let session = store
        .create_session(&project.id, "integration cache")
        .unwrap();
    let engine = Engine::new(store.clone());

    let first = run_integration_command(
        &engine,
        &project,
        &session,
        worktree.path().to_path_buf(),
        "echo x >> runs.log".to_string(),
    )
    .await
    .unwrap();
    assert!(!first.contains("integration cache hit"));
    let second = run_integration_command(
        &engine,
        &project,
        &session,
        worktree.path().to_path_buf(),
        "echo x >> runs.log".to_string(),
    )
    .await
    .unwrap();
    assert!(!second.contains("integration cache hit"));
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("runs.log"))
            .unwrap()
            .lines()
            .count(),
        2,
        "the default policy must never reuse an integration command"
    );

    engine
        .set_test_cache_policy(
            &project.id,
            vp_protocol::test_cache::TestCachePolicy {
                enabled: true,
                include_integration: true,
                ..vp_protocol::test_cache::TestCachePolicy::default()
            },
        )
        .await
        .unwrap();
    let executed = run_integration_command(
        &engine,
        &project,
        &session,
        worktree.path().to_path_buf(),
        "echo x >> runs.log".to_string(),
    )
    .await
    .unwrap();
    assert!(!executed.contains("integration cache hit"));
    let cached = run_integration_command(
        &engine,
        &project,
        &session,
        worktree.path().to_path_buf(),
        "echo x >> runs.log".to_string(),
    )
    .await
    .unwrap();
    assert!(cached.contains("integration cache hit"));
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("runs.log"))
            .unwrap()
            .lines()
            .count(),
        3,
        "the explicitly enabled integration cache must skip the second command"
    );
}

#[tokio::test]
async fn preempts_a_provider_stream_and_persists_reported_partial_usage() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(PreemptUsageProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "preempt stream".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "preempt stream").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        if events.iter().any(|event| {
            event.kind == "agent.message_delta" && event.payload["text"] == "partial answer"
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the provider stream did not start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result = service
        .preempt(&session.id, Some("test preemption"))
        .await
        .unwrap();
    assert!(result.preempted, "{}", result.detail);
    assert_eq!(result.reason, "explicit");
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "provider.preempted"
            && event.payload["reason"] == "explicit"
            && event.payload["phase"] == "provider"
            && event.payload["terminal"] == false
            && event.payload["partial_output_bytes"].as_u64().unwrap_or(0) > 0
    }));
    let terminal = events
        .iter()
        .find(|event| event.kind == "session.preempted")
        .expect("terminal preemption event");
    assert_eq!(terminal.payload["reason"], "explicit");
    assert_eq!(terminal.payload["phase"], "provider");
    assert_eq!(terminal.payload["terminal"], true);
    assert!(terminal.payload["checkpoint_id"].as_str().is_some());
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "agent.stream_retry"),
        "a preempted stream must not be retried or replayed"
    );
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.entries[0].input_tokens, 40);
    assert_eq!(ledger.entries[0].output_tokens, 5);
    assert_eq!(ledger.entries[0].cache_read_tokens, 8);
    assert!(
        !ledger.entries[0].estimated,
        "provider usage must be honest"
    );

    let preemption = engine.session_preemption(&session.id, 100).await.unwrap();
    assert!(preemption.preempted);
    assert_eq!(preemption.records.len(), 2);
    assert_eq!(preemption.by_phase.len(), 1);
    assert_eq!(preemption.by_phase[0].key, "provider");
    assert_eq!(preemption.by_phase[0].count, 1);
    assert_eq!(preemption.by_reason[0].key, "explicit");
    let live = service.preemption(&session.id, 100).await.unwrap();
    assert!(!live.active);
    assert!(live.preempted);
    let limited = engine.session_preemption(&session.id, 0).await.unwrap();
    assert_eq!(limited.records.len(), 1);

    // The history is derived from events, not a mutable projection: replay and
    // rebuild reproduce it.
    engine.rebuild_session(&session.id).await.unwrap();
    let replayed = engine.session_preemption(&session.id, 100).await.unwrap();
    assert_eq!(replayed.records.len(), 2);
    assert!(replayed.preempted);
}

#[tokio::test]
async fn wall_clock_budget_preempts_an_in_flight_provider_stream() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session_with_limits(project.id, "deadline".into(), None, None, Some(1))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    service.start(&session.id, "deadline").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "the wall-clock deadline fired too early"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let terminal = events
        .iter()
        .find(|event| event.kind == "session.preempted")
        .expect("wall-clock preemption event");
    assert_eq!(terminal.payload["reason"], "wall_clock");
    assert_eq!(terminal.payload["phase"], "provider");
    assert!(terminal.payload["created_at"].as_str().is_some());
    assert!(!service.cancel(&session.id));
}

#[tokio::test]
async fn disabled_policy_does_not_auto_preempt_a_healthy_run() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .set_preemption_policy(
            &project.id,
            vp_protocol::types::PreemptionPolicy {
                enabled: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let session = engine
        .create_session_with_limits(project.id, "no deadline".into(), None, None, Some(1))
        .await
        .unwrap();
    service.start(&session.id, "no deadline").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    let current = sessions
        .iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert_eq!(current.status, "running");
    assert!(service.cancel(&session.id));
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        !events.iter().any(|event| event.kind == "session.preempted"),
        "disabling the policy must disable automatic wall-clock preemption"
    );
}

#[tokio::test]
async fn tool_deadline_stops_only_the_tool_and_the_run_continues() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "call_sleep".into(),
                    name: "shell".into(),
                    arguments: json!({"command": "sleep 30"}).to_string(),
                })),
                Ok(StreamEvent::Done),
            ],
            vec![
                Ok(StreamEvent::TextDelta("recovered after deadline".into())),
                Ok(StreamEvent::Done),
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .set_preemption_policy(
            &project.id,
            vp_protocol::types::PreemptionPolicy {
                tool_deadline_secs: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "tool deadline".into(), None)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    service.start(&session.id, "tool deadline").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    assert!(started.elapsed() < Duration::from_secs(8));
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.preempted"
            && event.payload["reason"] == "tool_deadline"
            && event.payload["phase"] == "tool"
            && event.payload["side_effects_unknown"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["tool_call_id"] == "call_sleep"
            && event.payload["error_code"] == "DEADLINE_EXCEEDED"
    }));
    assert!(
        !events.iter().any(|event| event.kind == "session.preempted"),
        "a tool deadline is not a terminal session preemption"
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "recovered after deadline"
    }));
}

#[tokio::test]
async fn preempting_a_shell_kills_its_process_tree_and_marks_side_effects_unknown() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            Ok(StreamEvent::ToolCall(ToolCall {
                id: "call_tree".into(),
                name: "shell".into(),
                arguments:
                    json!({"command": "echo $$ > shell.pid; sleep 30 & echo $! > child.pid; wait"})
                        .to_string(),
            })),
            Ok(StreamEvent::Done),
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "kill tree".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "kill tree").await.unwrap();
    let worktree = std::path::PathBuf::from(session.worktree.as_deref().unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let (shell_pid, child_pid) = loop {
        let shell = std::fs::read_to_string(worktree.join("shell.pid"))
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok());
        let child = std::fs::read_to_string(worktree.join("child.pid"))
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok());
        if let (Some(shell), Some(child)) = (shell, child) {
            break (shell, child);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the shell process tree did not start"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let result = service
        .preempt(&session.id, Some("kill the tree"))
        .await
        .unwrap();
    assert!(result.preempted);
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    tokio::time::sleep(Duration::from_millis(200)).await;
    for pid in [shell_pid, child_pid] {
        // SAFETY: signal 0 only checks existence for a pid the test observed
        // in its own process group.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "process {pid} survived preemption");
    }
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.preempted"
            && event.payload["tool"] == "shell"
            && event.payload["side_effects_unknown"] == true
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["tool_call_id"] == "call_tree"
            && event.payload["error_code"] == "PREEMPTED"
    }));
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert!(
        queue.is_empty(),
        "preempted work must never enter the merge queue"
    );
}

#[tokio::test]
async fn explicit_preemption_creates_a_clean_checkpoint_for_resume() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "checkpoint".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "checkpoint").await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(service.preempt(&session.id, None).await.unwrap().preempted);
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    let checkpoint = events
        .iter()
        .find(|event| event.kind == "session.checkpoint")
        .expect("preemption checkpoint");
    let terminal = events
        .iter()
        .find(|event| event.kind == "session.preempted")
        .expect("terminal preemption");
    assert_eq!(
        terminal.payload["checkpoint_id"].as_str(),
        checkpoint.payload["checkpoint_id"].as_str()
    );
}

#[tokio::test]
async fn a_fast_run_finishes_normally_before_the_wall_clock_deadline() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![vec![
            Ok(StreamEvent::TextDelta("finished in time".into())),
            Ok(StreamEvent::Done),
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session_with_limits(project.id, "fast".into(), None, None, Some(5))
        .await
        .unwrap();
    service.start(&session.id, "fast").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        !events.iter().any(|event| event.kind == "session.preempted"),
        "a healthy run must not be preempted"
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "finished in time"
    }));
}

#[tokio::test]
async fn preemption_while_waiting_for_approval_records_the_queue_phase() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "call_approval".into(),
                    name: "shell".into(),
                    arguments: json!({"command": "echo approved > approved.txt"}).to_string(),
                })),
                Ok(StreamEvent::Done),
            ],
            vec![
                Ok(StreamEvent::TextDelta("not reached".into())),
                Ok(StreamEvent::Done),
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_approval_mode(&service, ApprovalMode::Strict);
    let session = engine
        .create_session(project.id, "approval queue".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "approval queue").await.unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        if events
            .iter()
            .any(|event| event.kind == "approval.requested")
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "the approval was not requested: {:?}",
                events
                    .iter()
                    .map(|event| (event.kind.clone(), event.payload.clone()))
                    .collect::<Vec<_>>()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        service
            .preempt(&session.id, Some("queue test"))
            .await
            .unwrap()
            .preempted
    );
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.preempt_requested" && event.payload["phase"] == "queue"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "session.preempted"
            && event.payload["phase"] == "queue"
            && event.payload["terminal"] == true
    }));
    assert!(
        !events.iter().any(|event| event.kind == "tool.started"),
        "a tool that never passed approval must not start"
    );
    let approvals = engine
        .list_approvals(Some(session.id.clone()))
        .await
        .unwrap();
    assert!(
        approvals
            .iter()
            .all(|approval| approval.status != "pending"),
        "preemption must resolve the pending approval"
    );
}

#[tokio::test]
async fn preempted_task_worktree_snapshot_preserves_untracked_work_without_moving_the_branch() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, _service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "task snapshot".into(), None)
        .await
        .unwrap();
    let worktree = std::path::PathBuf::from(session.worktree.as_deref().unwrap());
    std::fs::write(worktree.join("partial-task.txt"), "unfinished\n").unwrap();
    let head = |path: &std::path::Path| {
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap()
    };
    let before = String::from_utf8(head(&worktree).stdout).unwrap();
    let commit = engine
        .snapshot_preempted_task_worktree(&session.id, "T1", &worktree, "test preemption")
        .await
        .unwrap();
    assert_eq!(commit.len(), 40);
    let after = String::from_utf8(head(&worktree).stdout).unwrap();
    assert_eq!(before, after, "the task branch must not move");
    let exists = std::process::Command::new("git")
        .args(["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .current_dir(&worktree)
        .status()
        .unwrap();
    assert!(
        exists.success(),
        "the preemption snapshot commit must exist"
    );
    assert!(worktree.join("partial-task.txt").is_file());
}

#[tokio::test]
async fn preemption_estimates_partial_usage_when_the_provider_reported_none() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(PartialSlowProvider);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "estimated partial".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "estimated partial")
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        if events
            .iter()
            .any(|event| event.kind == "agent.message_delta" && event.payload["text"] == "partial")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the partial stream did not start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(service.preempt(&session.id, None).await.unwrap().preempted);
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let provider_event = events
        .iter()
        .find(|event| event.kind == "provider.preempted")
        .expect("provider preemption event");
    assert_eq!(provider_event.payload["usage_estimated"], true);
    assert!(
        provider_event.payload["partial_input_tokens"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(
        provider_event.payload["partial_output_tokens"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    let ledger = engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(ledger.entries.len(), 1);
    assert!(ledger.entries[0].estimated);
    assert!(ledger.entries[0].input_tokens > 0);
    assert!(ledger.entries[0].output_tokens > 0);
}

#[tokio::test]
async fn preempting_a_paused_run_cancels_instead_of_leaving_it_paused() {
    let dir = TempDir::new().unwrap();
    let provider = Arc::new(PausingProvider {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let provider_trait: Arc<dyn Provider> = provider.clone();
    let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
    let session = engine
        .create_session(project.id, "paused preempt".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "paused preempt").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
        .await
        .unwrap();
    assert!(service.pause(&session.id).await.unwrap());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "paused");
    assert!(
        service
            .preempt(&session.id, Some("stop paused"))
            .await
            .unwrap()
            .preempted
    );
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "cancelled");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.preempted" && event.payload["reason"] == "explicit"
    }));
    assert!(
        !events.iter().any(|event| event.kind == "agent.resumed"),
        "preemption must not resume a paused run"
    );
    let preemption = engine.session_preemption(&session.id, 100).await.unwrap();
    assert!(preemption.preempted);
}

#[tokio::test]
async fn preempting_a_session_waiting_for_admission_records_the_queue_phase() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
    let (engine, service, project) = test_service_with_provider_cap(&dir, provider, 1);
    let first = engine
        .create_session(project.id.clone(), "first".into(), None)
        .await
        .unwrap();
    let second = engine
        .create_session(project.id, "second".into(), None)
        .await
        .unwrap();
    service.start(&first.id, "first").await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    service.start(&second.id, "second").await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sessions = engine
        .list_sessions(Some(second.project_id.clone()))
        .await
        .unwrap();
    let queued = sessions
        .iter()
        .find(|candidate| candidate.id == second.id)
        .expect("second session");
    assert_eq!(queued.status, "queued");
    assert!(
        service
            .preempt(&second.id, Some("leave the queue"))
            .await
            .unwrap()
            .preempted
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let sessions = engine
            .list_sessions(Some(second.project_id.clone()))
            .await
            .unwrap();
        let current = sessions
            .iter()
            .find(|candidate| candidate.id == second.id)
            .unwrap();
        if current.status == "cancelled" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queued session did not cancel: {}",
            current.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let events = engine.list_events(second.id.clone(), 0, 200).await.unwrap();
    assert!(
        {
            let found = events.iter().any(|event| {
                event.kind == "session.preempted" && event.payload["phase"] == "queue"
            });
            if !found {
                eprintln!(
                    "events: {:?}",
                    events
                        .iter()
                        .map(|event| (event.kind.clone(), event.payload.clone()))
                        .collect::<Vec<_>>()
                );
            }
            found
        },
        "missing queue preemption"
    );
    assert!(
        !events.iter().any(|event| event.kind == "agent.started"),
        "a queued session must not start after preemption"
    );
    assert!(service.cancel(&first.id));
    let _ = wait_until_idle(&engine, &first).await;
}

#[tokio::test]
async fn a_tool_deadline_aborts_a_hanging_local_http_request_and_continues() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(ScriptedStreamProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                Ok(StreamEvent::ToolCall(ToolCall {
                    id: "call_http".into(),
                    name: "local_http".into(),
                    arguments: json!({
                        "url": format!("http://{address}/hang"),
                        "timeout_secs": 60
                    })
                    .to_string(),
                })),
                Ok(StreamEvent::Done),
            ],
            vec![
                Ok(StreamEvent::TextDelta("http deadline recovered".into())),
                Ok(StreamEvent::Done),
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    engine
        .set_preemption_policy(
            &project.id,
            vp_protocol::types::PreemptionPolicy {
                tool_deadline_secs: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "http deadline".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "http deadline").await.unwrap();
    let finished = wait_until_idle(&engine, &session).await;
    assert_eq!(finished.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.preempted"
            && event.payload["tool"] == "local_http"
            && event.payload["reason"] == "tool_deadline"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["tool_call_id"] == "call_http"
            && event.payload["error_code"] == "DEADLINE_EXCEEDED"
    }));
    assert!(events.iter().any(|event| {
        event.kind == "agent.message" && event.payload["text"] == "http deadline recovered"
    }));
}

struct ParallelFakeTool {
    name: &'static str,
    delay: std::time::Duration,
    completed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

struct ParallelDropFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for ParallelDropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl crate::tools::AgentTool for ParallelFakeTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "parallel deadline test tool"
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        _input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let _flag = ParallelDropFlag(self.dropped.clone());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.completed.store(true, Ordering::SeqCst);
        Ok(ToolOutput::ok(format!("{}:done", self.name)))
    }
}

fn parallel_fake(
    name: &'static str,
    delay: std::time::Duration,
) -> (
    std::sync::Arc<ParallelFakeTool>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        std::sync::Arc::new(ParallelFakeTool {
            name,
            delay,
            completed: completed.clone(),
            dropped: dropped.clone(),
        }),
        completed,
        dropped,
    )
}

#[tokio::test]
async fn parallel_tool_deadline_aborts_only_the_slow_tool_and_drops_its_work() {
    let run = crate::preemption::Token::disabled();
    let fast = parallel_fake("fast_read", std::time::Duration::ZERO);
    let slow = parallel_fake("slow_read", std::time::Duration::from_secs(5));
    let fast_token =
        crate::preemption::Token::tool_child(&run, Some(std::time::Duration::from_secs(2)));
    let slow_token =
        crate::preemption::Token::tool_child(&run, Some(std::time::Duration::from_millis(100)));
    let started = Instant::now();
    let (fast_result, slow_result) = tokio::join!(
        run_tool_with_token_deadline(&fast_token, "fast_read", async {
            fast.0.execute(&test_context(), serde_json::json!({})).await
        }),
        run_tool_with_token_deadline(&slow_token, "slow_read", async {
            slow.0.execute(&test_context(), serde_json::json!({})).await
        }),
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "slow tool was not aborted promptly: {elapsed:?}"
    );
    assert!(!fast_result.0.is_error);
    assert!(fast_result.0.content.contains("fast_read:done"));
    assert!(fast.1.load(Ordering::SeqCst));
    assert!(slow_result.0.is_error);
    assert!(
        slow_result.0.content.contains("DEADLINE_EXCEEDED"),
        "{}",
        slow_result.0.content
    );
    let snapshot = slow_result.1.expect("typed deadline snapshot");
    assert_eq!(snapshot.reason, crate::preemption::Reason::ToolDeadline);
    assert_eq!(snapshot.phase, crate::preemption::Phase::Tool);
    assert!(!slow.1.load(Ordering::SeqCst), "slow work completed");
    assert!(slow.2.load(Ordering::SeqCst), "slow work was not dropped");
    assert!(!run.is_preempted());
    assert!(!fast_token.is_preempted());
}

#[tokio::test]
async fn parallel_tool_run_preemption_still_aborts_every_tool() {
    let run = crate::preemption::Token::disabled();
    let first = parallel_fake("first_read", std::time::Duration::from_secs(5));
    let second = parallel_fake("second_read", std::time::Duration::from_secs(5));
    let first_token =
        crate::preemption::Token::tool_child(&run, Some(std::time::Duration::from_secs(30)));
    let second_token =
        crate::preemption::Token::tool_child(&run, Some(std::time::Duration::from_secs(30)));
    let trigger = run.clone();
    let preemptor = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        trigger.trigger(
            crate::preemption::Reason::Explicit,
            crate::preemption::Phase::Tool,
            false,
        );
    });
    let (first_result, second_result) = tokio::join!(
        run_tool_with_token_deadline(&first_token, "first_read", async {
            first
                .0
                .execute(&test_context(), serde_json::json!({}))
                .await
        }),
        run_tool_with_token_deadline(&second_token, "second_read", async {
            second
                .0
                .execute(&test_context(), serde_json::json!({}))
                .await
        }),
    );
    preemptor.await.unwrap();
    assert!(first_result.0.is_error && second_result.0.is_error);
    assert!(first_result.0.content.contains("PREEMPTED"));
    assert!(second_result.0.content.contains("PREEMPTED"));
    assert_eq!(
        first_result.1.expect("first snapshot").reason,
        crate::preemption::Reason::Explicit
    );
    assert!(!first.1.load(Ordering::SeqCst));
    assert!(!second.1.load(Ordering::SeqCst));
    assert!(first.2.load(Ordering::SeqCst) && second.2.load(Ordering::SeqCst));
}

#[tokio::test]
async fn frozen_tool_bundle_matches_the_role_matrix_and_stays_stable() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let config = service.config.read().unwrap().as_ref().unwrap().clone();
    let no_mcp: Vec<McpToolBinding> = Vec::new();

    let mut leader = help_spec(AgentRole::Leader, AgentProfile::Coding);
    leader.allow_delegation = true;
    let leader_bundle = frozen_tool_bundle(&config, &leader, &no_mcp);
    assert_eq!(
        leader_bundle.kind(),
        crate::tool_bundle::ToolBundleKind::Leader
    );
    assert!(leader_bundle.builtin_count() <= crate::tool_bundle::LEADER_TOOL_LIMIT);
    assert_eq!(leader_bundle.builtin_count(), 49);
    assert!(leader_bundle.contains("submit_team_plan"));
    assert!(leader_bundle.contains("delegate"));
    assert!(leader_bundle.contains("browser"));
    assert!(leader_bundle.contains("computer"));
    assert!(!leader_bundle.contains("submit_verdict"));
    assert!(leader_bundle.within_target() == Some(true));
    assert_eq!(leader_bundle.bundle_hash.len(), 64);
    assert_eq!(leader_bundle.tools_hash.len(), 64);

    let mut second = help_spec(AgentRole::Leader, AgentProfile::Coding);
    second.allow_delegation = true;
    let second_bundle = frozen_tool_bundle(&config, &second, &no_mcp);
    assert_eq!(leader_bundle.tool_names(), second_bundle.tool_names());
    assert_eq!(leader_bundle.bundle_hash, second_bundle.bundle_hash);
    assert_eq!(leader_bundle.tools_hash, second_bundle.tools_hash);

    let mut coding = help_spec(AgentRole::Worker, AgentProfile::Coding);
    coding.allow_delegation = true;
    coding.task_kind = Some("implementation".into());
    let coding_bundle = frozen_tool_bundle(&config, &coding, &no_mcp);
    assert_eq!(
        coding_bundle.kind(),
        crate::tool_bundle::ToolBundleKind::Coding
    );
    assert!(coding_bundle.builtin_count() <= crate::tool_bundle::CODING_TOOL_LIMIT);
    assert!(coding_bundle.contains("write_file"));
    assert!(coding_bundle.contains("delegate"));
    assert!(coding_bundle.contains("run_project_task"));
    assert!(coding_bundle.contains("browser"));
    for forbidden in [
        "submit_team_plan",
        "todo_write",
        "submit_verdict",
        "task_control",
    ] {
        assert!(
            !coding_bundle.contains(forbidden),
            "coding Worker must not advertise {forbidden}"
        );
    }

    let mut labour = help_spec(AgentRole::Labour, AgentProfile::Coding);
    labour.allow_delegation = false;
    labour.task_kind = Some("implementation".into());
    let labour_bundle = frozen_tool_bundle(&config, &labour, &no_mcp);
    assert!(!labour_bundle.contains("delegate"));

    let mut verification = help_spec(AgentRole::Verifier, AgentProfile::Verification);
    verification.task_kind = Some("review".into());
    let verification_bundle = frozen_tool_bundle(&config, &verification, &no_mcp);
    assert_eq!(
        verification_bundle.kind(),
        crate::tool_bundle::ToolBundleKind::Verification
    );
    assert!(verification_bundle.contains("submit_verdict"));
    for forbidden in [
        "write_file",
        "edit_file",
        "apply_patch",
        "delegate",
        "todo_write",
    ] {
        assert!(!verification_bundle.contains(forbidden));
    }

    let mut research = help_spec(AgentRole::Worker, AgentProfile::Research);
    research.task_kind = Some("research".into());
    let research_bundle = frozen_tool_bundle(&config, &research, &no_mcp);
    assert_eq!(
        research_bundle.kind(),
        crate::tool_bundle::ToolBundleKind::Research
    );
    assert!(research_bundle.contains("web_search"));
    assert!(!research_bundle.contains("shell"));
    assert!(!research_bundle.contains("write_file"));

    let mut minimal = help_spec(AgentRole::Worker, AgentProfile::Coding);
    minimal.task_kind = Some("trivial".into());
    let minimal_bundle = frozen_tool_bundle(&config, &minimal, &no_mcp);
    assert_eq!(
        minimal_bundle.kind(),
        crate::tool_bundle::ToolBundleKind::Minimal
    );
    assert_eq!(minimal_bundle.builtin_count(), 6);
    assert!(minimal_bundle.within_target() == Some(true));

    let mut unknown = help_spec(AgentRole::Worker, AgentProfile::Coding);
    unknown.task_kind = Some("not-a-real-kind".into());
    let unknown_bundle = frozen_tool_bundle(&config, &unknown, &no_mcp);
    assert_eq!(
        unknown_bundle.kind(),
        crate::tool_bundle::ToolBundleKind::Full
    );
    assert!(unknown_bundle.selection.fallback);
    assert!(unknown_bundle.builtin_count() > minimal_bundle.builtin_count());
}

#[tokio::test]
async fn leader_run_advertises_the_partitioned_bundle_and_telemetry() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
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
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "bundle leader".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "bundle leader").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    let names = {
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        let names = recorded[0]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        assert!(names.len() <= crate::tool_bundle::LEADER_TOOL_LIMIT);
        for required in [
            "read_file",
            "delegate",
            "submit_team_plan",
            "todo_write",
            "tool_help",
        ] {
            assert!(names.contains(&required.to_string()), "missing {required}");
        }
        assert!(!names.contains(&"submit_verdict".to_string()));
        for request in recorded.iter() {
            let turn_names = request
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            assert_eq!(turn_names, names, "tool schemas must not churn per turn");
        }
        let frozen_hash = crate::tool_bundle::canonical_tools_hash(&recorded[0].tools);
        let second_hash = crate::tool_bundle::canonical_tools_hash(&recorded[1].tools);
        assert_eq!(frozen_hash, second_hash);
        names
    };

    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix");
    assert_eq!(prefix.payload["tool_bundle"], "leader");
    assert_eq!(prefix.payload["tool_bundle_frozen"], true);
    assert_eq!(prefix.payload["tool_bundle_within_target"], true);
    assert_eq!(prefix.payload["tool_bundle_fallback"], false);
    assert_eq!(prefix.payload["tool_bundle_tool_count"], names.len());
    assert!(
        prefix.payload["tool_bundle_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
    );
    assert!(
        prefix.payload["tool_bundle_mcp_count"]
            .as_u64()
            .is_some_and(|count| count == 0)
    );
    let cache = events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache telemetry");
    assert_eq!(cache.payload["tool_bundle"], "leader");
    assert!(cache.payload["tool_bundle_hash"].as_str().is_some());
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "context.tool_bundle")
            .count(),
        1,
        "the manifest is emitted once per run, not per turn"
    );
    let manifest = events
        .iter()
        .find(|event| event.kind == "context.tool_bundle")
        .expect("tool bundle manifest");
    assert_eq!(manifest.payload["bundle"], "leader");
    assert_eq!(manifest.payload["frozen"], true);
    assert_eq!(manifest.payload["builtin_count"], names.len());
    assert_eq!(
        manifest.payload["tools"].as_array().unwrap().len(),
        names.len()
    );
    assert!(!manifest.payload.to_string().contains("description for"));
    for tool in manifest.payload["tools"].as_array().unwrap() {
        assert!(tool["schema_hash"].as_str().unwrap().len() == 64);
        assert!(tool["category"].as_str().is_some());
    }
}

#[tokio::test]
async fn tool_bundle_now_reports_minimal_worker_and_catalog_without_a_run() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "bundle inspect".into(), None)
        .await
        .unwrap();
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let worker = engine
        .create_agent(
            &session.id,
            "worker",
            "Preview Worker",
            None,
            Some(leader_id),
            "test",
        )
        .await
        .unwrap();
    engine
        .set_agent_profile(&worker.id, "coding")
        .await
        .unwrap();
    let result = service
        .tool_bundle_now(&session.id, &["worker".to_string()], Some("trivial"), true)
        .await
        .unwrap();
    assert_eq!(result.roles.len(), 1);
    let worker = &result.roles[0];
    assert_eq!(worker.role, "worker");
    assert_eq!(worker.bundle, "minimal");
    assert_eq!(worker.builtin_count, 6);
    assert_eq!(
        worker.target_limit,
        Some(crate::tool_bundle::MINIMAL_TOOL_LIMIT)
    );
    assert_eq!(worker.within_target, Some(true));
    assert!(!worker.fallback);
    assert_eq!(worker.tools.len(), 6);
    assert!(worker.last_run.is_none());
    for tool in &worker.tools {
        assert!(
            tool.category != "unbundled",
            "missing metadata for {}",
            tool.name
        );
    }
    assert_eq!(result.catalog.len(), 6);
    let leader = result
        .catalog
        .iter()
        .find(|row| row.bundle == "leader")
        .unwrap();
    assert_eq!(leader.target_limit, Some(50));

    let snapshot = service
        .tool_bundle_now(&session.id, &[], None, false)
        .await
        .unwrap();
    assert!(snapshot.roles.iter().any(|role| role.role == "leader"));
    assert!(snapshot.roles.iter().all(|role| role.tools.is_empty()));
    let default_leader = snapshot
        .roles
        .iter()
        .find(|role| role.role == "leader")
        .unwrap();
    assert_eq!(default_leader.bundle, "leader");
    assert_eq!(default_leader.builtin_count, 47);
}

#[tokio::test]
async fn cache_plan_reports_the_frozen_bundle_for_each_role() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "bundle plan".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let plan = service
        .cache_plan_now(&session.id, &["leader".to_string()])
        .await
        .unwrap();
    assert_eq!(plan.roles.len(), 1);
    let leader = &plan.roles[0];
    assert_eq!(leader.tool_bundle, "leader");
    assert_eq!(leader.tool_bundle_tool_count, 47);
    assert_eq!(leader.tool_bundle_within_target, Some(true));
    assert!(!leader.tool_bundle_fallback);
    assert_eq!(leader.tool_bundle_hash.len(), 64);
    assert!(leader.tool_count >= leader.tool_bundle_tool_count);
}

#[tokio::test]
async fn verifier_dispatch_denies_a_manual_write_outside_role_policy() {
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
                        "objective": "write verified.txt",
                        "write_scope": ["verified.txt"]
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
                        "path": "verified.txt",
                        "content": "approved change\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker wrote".into()),
                StreamEvent::Done,
            ],
            // The Verifier manually requests a write. The schema is absent and
            // role policy must reject the dispatch before approval or disk I/O.
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "call_verifier_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "verifier-write.txt",
                        "content": "must not exist\n"
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
                        "approved": false,
                        "summary": "do not merge yet"
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
        .create_session(project.id, "verifier bundle".into(), None)
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
    service.start(&session.id, "verifier bundle").await.unwrap();
    for _ in 0..150 {
        let agents = engine.list_agents(&session.id).await.unwrap();
        if agents.iter().any(|agent| {
            agent.role == "verifier" && matches!(agent.status.as_str(), "idle" | "failed")
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    approver.abort();

    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    let denied = events
        .iter()
        .find(|event| {
            event.kind == "tool.finished"
                && event.payload["agent_role"] == "verifier"
                && event.payload["name"] == "write_file"
        })
        .expect("verifier write attempt is recorded");
    assert_eq!(denied.payload["is_error"], true);
    assert!(
        denied.payload["output"]
            .as_str()
            .is_some_and(|output| output.contains("not available to verifier")),
        "{}",
        denied.payload["output"]
    );
    assert!(
        !events.iter().any(|event| {
            event.kind == "approval.requested"
                && event.payload["agent_role"] == "verifier"
                && event.payload["tool"] == "write_file"
        }),
        "role denial must not ask the user for approval"
    );
    // The verifier's detached clone never receives the file either.
    let verifier = engine
        .list_agents(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.role == "verifier")
        .expect("verifier");
    if let Some(worktree) = verifier.worktree {
        assert!(
            !std::path::Path::new(&worktree)
                .join("verifier-write.txt")
                .exists()
        );
    }
}

#[tokio::test]
async fn plan_task_kind_selects_minimal_coding_and_verification_bundles_at_runtime() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(BundlePlanProvider {
        requests: requests.clone(),
        leader_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "bundle plan runtime".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service
        .start(&session.id, "bundle plan runtime")
        .await
        .unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    let worktree = session.worktree.clone().unwrap();
    assert!(
        std::path::Path::new(&worktree)
            .join("bundle-note.txt")
            .exists(),
        "the minimal bundle must still be able to write the one-file task"
    );
    {
        let recorded = requests.lock().unwrap();
        assert!(!recorded.is_empty());
        let leader_requests = recorded
            .iter()
            .filter(|request| {
                request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "submit_team_plan")
            })
            .collect::<Vec<_>>();
        assert!(!leader_requests.is_empty());
        for request in &leader_requests {
            let names = request
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>();
            assert!(names.len() <= crate::tool_bundle::LEADER_TOOL_LIMIT);
            assert!(!names.contains(&"submit_verdict".to_string()));
        }
        let worker_requests = recorded
            .iter()
            .filter(|request| {
                request.tools.iter().any(|tool| tool.name == "write_file")
                    && !request
                        .tools
                        .iter()
                        .any(|tool| tool.name == "submit_team_plan")
            })
            .collect::<Vec<_>>();
        assert_eq!(worker_requests.len(), 2, "write turn plus final text turn");
        for request in &worker_requests {
            let names = request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>();
            assert_eq!(names.len(), 6, "trivial task must use the minimal bundle");
            assert_eq!(
                names,
                vec![
                    "edit_file",
                    "list_dir",
                    "read_file",
                    "read_tool_output",
                    "shell",
                    "write_file",
                ]
            );
        }
        let verifier_requests = recorded
            .iter()
            .filter(|request| {
                request
                    .tools
                    .iter()
                    .any(|tool| tool.name == "submit_verdict")
            })
            .collect::<Vec<_>>();
        assert_eq!(verifier_requests.len(), 1);
        let verifier_names = verifier_requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(!verifier_names.contains(&"write_file"));
        assert!(!verifier_names.contains(&"edit_file"));
        assert!(!verifier_names.contains(&"delegate"));
        assert!(verifier_names.contains(&"shell"));
    }

    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    let worker_prefix = events
        .iter()
        .find(|event| {
            event.kind == "context.prompt_prefix" && event.payload["agent_role"] == "worker"
        })
        .expect("worker prompt prefix");
    assert_eq!(worker_prefix.payload["tool_bundle"], "minimal");
    assert_eq!(worker_prefix.payload["tool_bundle_tool_count"], 6);
    assert_eq!(worker_prefix.payload["tool_bundle_within_target"], true);
    let verifier_prefix = events
        .iter()
        .find(|event| {
            event.kind == "context.prompt_prefix" && event.payload["agent_role"] == "verifier"
        })
        .expect("verifier prompt prefix");
    assert_eq!(verifier_prefix.payload["tool_bundle"], "verification");
    assert!(
        verifier_prefix.payload["tool_bundle_tool_count"]
            .as_u64()
            .unwrap_or(0)
            < crate::tool_bundle::LEADER_TOOL_LIMIT as u64
    );
    let leader_prefix = events
        .iter()
        .filter(|event| {
            event.kind == "context.prompt_prefix" && event.payload["agent_role"] == "leader"
        })
        .count();
    assert!(leader_prefix >= 1);
}

#[tokio::test]
async fn bundle_catalog_is_exposed_through_agent_info_without_warnings() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let catalog = service.tool_bundle_catalog();
    assert_eq!(catalog.bundles.len(), 6);
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    let names = catalog
        .bundles
        .iter()
        .map(|row| row.bundle.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "leader",
            "coding",
            "research",
            "verification",
            "minimal",
            "full"
        ]
    );
    let info = service.info();
    assert_eq!(info.tool_bundles.len(), 6);
    assert_eq!(info.tool_bundles[4].bundle, "minimal");
    assert_eq!(info.tool_bundles[4].target_limit, Some(12));
}

#[tokio::test]
async fn leader_bundle_hash_is_stable_across_sessions_and_runs() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let mut prefix_hashes = Vec::new();
    for goal in ["bundle session one", "bundle session two"] {
        let session = engine
            .create_session(project.id.clone(), goal.into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, goal).await.unwrap();
        let _ = wait_until_idle(&engine, &session).await;
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        let prefix = events
            .iter()
            .find(|event| event.kind == "context.prompt_prefix")
            .expect("prompt prefix");
        assert_eq!(prefix.payload["tool_bundle"], "leader");
        prefix_hashes.push(
            prefix.payload["tool_bundle_hash"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(prefix_hashes[0], prefix_hashes[1]);
    let recorded = requests.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(
        crate::tool_bundle::canonical_tools_hash(&recorded[0].tools),
        crate::tool_bundle::canonical_tools_hash(&recorded[1].tools)
    );
    assert_eq!(
        recorded[0]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        recorded[1]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn tool_bundle_matrix_covers_standard_combinations_without_session_state() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let matrix = service.tool_bundle_matrix(&[]).unwrap();
    assert_eq!(matrix.task_kinds.len(), 6);
    assert!(matrix.warnings.is_empty(), "{:?}", matrix.warnings);
    assert_eq!(matrix.rows.len(), 10 * 6);
    let find = |role: &str, profile: &str, kind: Option<&str>| {
        matrix
            .rows
            .iter()
            .find(|row| {
                row.role == role && row.profile == profile && row.task_kind.as_deref() == kind
            })
            .unwrap_or_else(|| panic!("missing row {role}/{profile}/{kind:?}"))
    };
    let leader = find("leader", "generic", None);
    assert_eq!(leader.bundle, "leader");
    assert_eq!(leader.builtin_count, 47);
    assert_eq!(leader.within_target, Some(true));
    assert!(!leader.fallback);
    let manager = find("manager", "coding", None);
    assert_eq!(manager.bundle, "leader");
    assert_eq!(manager.builtin_count, 36);
    assert_eq!(manager.mcp_count, 0);
    let worker_coding = find("worker", "coding", Some("implementation"));
    assert_eq!(worker_coding.bundle, "coding");
    assert_eq!(worker_coding.builtin_count, 38);
    assert_eq!(worker_coding.within_target, Some(true));
    let worker_minimal = find("worker", "coding", Some("trivial"));
    assert_eq!(worker_minimal.bundle, "minimal");
    assert_eq!(worker_minimal.builtin_count, 6);
    let worker_research = find("worker", "research", Some("research"));
    assert_eq!(worker_research.bundle, "research");
    assert_eq!(worker_research.builtin_count, 34);
    let verifier = find("verifier", "verification", Some("implementation"));
    assert_eq!(verifier.bundle, "verification");
    assert_eq!(verifier.builtin_count, 38);
    let unknown = find("worker", "coding", Some("unknown-task-kind"));
    assert_eq!(unknown.bundle, "full");
    assert!(unknown.fallback);
    let labour = find("labour", "coding", Some("implementation"));
    assert_eq!(labour.bundle, "coding");
    assert_eq!(labour.builtin_count, 36);
    let scout = find("scout", "research", None);
    assert_eq!(scout.bundle, "research");
    assert_eq!(scout.builtin_count, 20);
    assert_eq!(scout.requested_count, 33);
    assert_eq!(scout.omitted_count, 13);
    assert!(scout.omitted_count > 0, "Scout has role-only omissions");
    for row in &matrix.rows {
        assert_eq!(row.bundle_hash.len(), 64);
        assert_eq!(row.tools_hash.len(), 64);
        assert_eq!(row.total_count, row.builtin_count + row.mcp_count);
        assert_eq!(
            row.requested_count,
            row.builtin_count + row.omitted_count,
            "{row:?}"
        );
    }
    let custom = service
        .tool_bundle_matrix(&["trivial".to_string(), "custom-unknown".to_string()])
        .unwrap();
    assert_eq!(custom.task_kinds, vec!["trivial", "custom-unknown"]);
    assert_eq!(custom.rows.len(), 10 * 2);
    assert!(
        custom
            .rows
            .iter()
            .filter(|row| row.task_kind.as_deref() == Some("custom-unknown"))
            .all(|row| row.bundle == "full" || row.role != "worker")
    );
}

#[tokio::test]
async fn tool_bundle_history_is_bounded_newest_first_and_role_filtered() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "bundle history".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for prompt in ["bundle history one", "bundle history two"] {
        service.start(&session.id, prompt).await.unwrap();
        let _ = wait_until_idle(&engine, &session).await;
    }
    let history = service
        .tool_bundle_history(&session.id, Some("leader"), Some(50))
        .await
        .unwrap();
    assert_eq!(history.entries.len(), 2);
    assert!(history.entries[0].seq > history.entries[1].seq);
    for entry in &history.entries {
        assert_eq!(entry.role, "leader");
        assert_eq!(entry.bundle, "leader");
        assert_eq!(entry.bundle_hash.len(), 64);
        assert_eq!(entry.tools_hash.len(), 64);
        assert_eq!(entry.tool_count, 47);
        assert!(!entry.fallback);
        assert!(!entry.created_at.is_empty());
    }
    let worker = service
        .tool_bundle_history(&session.id, Some("worker"), None)
        .await
        .unwrap();
    assert!(worker.entries.is_empty());
    let capped = service
        .tool_bundle_history(&session.id, None, Some(1))
        .await
        .unwrap();
    assert_eq!(capped.entries.len(), 1);
    assert_eq!(capped.entries[0].seq, history.entries[0].seq);
    assert!(
        service
            .tool_bundle_history(&session.id, Some("nope"), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn tool_bundle_diff_is_deterministic_and_bounded() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let diff = service
        .tool_bundle_diff("leader", "minimal", Some("leader"), Some("generic"), None)
        .unwrap();
    assert_eq!(diff.from_bundle, "leader");
    assert_eq!(diff.to_bundle, "minimal");
    assert_eq!(diff.from_count, 47);
    assert_eq!(diff.to_count, 6);
    assert!(diff.changed);
    assert!(diff.added.is_empty());
    assert_eq!(diff.removed.len(), 41);
    assert_eq!(diff.shared.len(), 6);
    let mut sorted = diff.removed.clone();
    sorted.sort();
    assert_eq!(diff.removed, sorted);
    assert_ne!(diff.from_hash, diff.to_hash);
    let again = service
        .tool_bundle_diff("leader", "minimal", Some("leader"), Some("generic"), None)
        .unwrap();
    assert_eq!(diff, again);

    let reverse = service
        .tool_bundle_diff("minimal", "leader", Some("leader"), Some("generic"), None)
        .unwrap();
    assert!(reverse.added.contains(&"delegate".to_string()));
    assert!(reverse.removed.is_empty());
    assert_eq!(reverse.shared, diff.shared);

    let coding = service
        .tool_bundle_diff(
            "minimal",
            "coding",
            Some("worker"),
            Some("coding"),
            Some("implementation"),
        )
        .unwrap();
    assert!(coding.added.contains(&"grep".to_string()));
    assert!(coding.added.contains(&"delegate".to_string()));
    assert!(coding.shared.contains(&"write_file".to_string()));

    assert!(
        service
            .tool_bundle_diff("leader", "does-not-exist", None, None, None)
            .is_err()
    );
    assert!(
        service
            .tool_bundle_diff("leader", "minimal", Some("nope"), None, None)
            .is_err()
    );
}

#[tokio::test]
async fn dynamic_mcp_tools_are_sorted_frozen_and_manifested() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("ordered_mcp_server.py");
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
        result = {"tools": [
            {"name": "zeta", "description": "Zeta tool", "inputSchema": {"type": "object"}},
            {"name": "alpha", "description": "Alpha tool", "inputSchema": {"type": "object"}}
        ]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message.get("id"), "result": result}), flush=True)
"#,
    )
    .unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "mcp_read".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("mcp turn two".into()),
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
        .create_session(project.id, "ordered mcp".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    service.start(&session.id, "use ordered mcp").await.unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    {
        let recorded = requests.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        let names = recorded[0]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let alpha = names
            .iter()
            .position(|name| name == "mcp_mock_alpha")
            .unwrap();
        let zeta = names
            .iter()
            .position(|name| name == "mcp_mock_zeta")
            .unwrap();
        assert!(alpha < zeta, "MCP tools must be name-sorted");
        let first_mcp = names
            .iter()
            .position(|name| name.starts_with("mcp_"))
            .unwrap();
        assert!(
            names[..first_mcp]
                .iter()
                .all(|name| !name.starts_with("mcp_")),
            "MCP tools are appended once after built-ins"
        );
        assert_eq!(first_mcp, 47);
        assert_eq!(&names[first_mcp..], ["mcp_mock_alpha", "mcp_mock_zeta"]);
        assert_eq!(
            recorded[0]
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            recorded[1]
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
            "MCP schemas must not churn between turns"
        );
    }

    let events = engine
        .list_events(session.id.clone(), 0, 250)
        .await
        .unwrap();
    let prefix = events
        .iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix");
    assert_eq!(prefix.payload["tool_bundle"], "leader");
    assert_eq!(prefix.payload["tool_bundle_mcp_count"], 2);
    assert_eq!(prefix.payload["tool_bundle_tool_count"], 47);
    assert_eq!(prefix.payload["tool_count"], 49);
    let manifest = events
        .iter()
        .find(|event| event.kind == "context.tool_bundle")
        .expect("bundle manifest");
    assert_eq!(manifest.payload["mcp_count"], 2);
    let mcp_tools = manifest.payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["name"].as_str().unwrap_or("").starts_with("mcp_"))
        .collect::<Vec<_>>();
    assert_eq!(mcp_tools.len(), 2);
    assert!(mcp_tools.iter().all(|tool| tool["category"] == "mcp"));
}

#[tokio::test]
async fn cache_plan_uses_the_stored_plan_task_kind_for_a_worker() {
    let dir = TempDir::new().unwrap();
    let (engine, service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id, "cache plan task kind".into(), None)
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "trivial cache plan".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "make the tiny change".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![PlanTask {
            id: "T1".into(),
            department: "impl".into(),
            title: "Tiny".into(),
            kind: "trivial".into(),
            priority: 0,
            profile: None,
            deps: Vec::new(),
            write_scope: vec!["note.txt".into()],
            acceptance: vec!["note.txt exists".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let worker = engine
        .create_agent(
            &session.id,
            "worker",
            "Planned Worker",
            None,
            Some(leader_id),
            "test",
        )
        .await
        .unwrap();
    engine
        .set_agent_profile(&worker.id, "coding")
        .await
        .unwrap();
    engine
        .set_agent_task(&worker.id, Some("T1".into()), Some("impl".into()))
        .await
        .unwrap();
    let plan_result = service
        .cache_plan_now(&session.id, &["worker".to_string()])
        .await
        .unwrap();
    let worker_plan = &plan_result.roles[0];
    assert_eq!(worker_plan.role, "worker");
    assert_eq!(worker_plan.tool_bundle, "minimal");
    assert_eq!(worker_plan.tool_bundle_tool_count, 6);
    assert_eq!(worker_plan.tool_bundle_mcp_count, 0);
    assert_eq!(worker_plan.tool_bundle_target_limit, Some(12));
    assert_eq!(worker_plan.tool_bundle_within_target, Some(true));
    assert!(!worker_plan.tool_bundle_fallback);
    let encoded = serde_json::to_value(&plan_result).unwrap();
    assert_eq!(encoded["roles"][0]["tool_bundle"], "minimal");
    assert_eq!(encoded["roles"][0]["tool_bundle_tool_count"], 6);
}
fn test_context() -> ToolContext {
    ToolContext {
        root: std::path::PathBuf::from("/tmp"),
        project_root: std::path::PathBuf::from("/tmp"),
        sandbox_root: std::path::PathBuf::from("/tmp"),
        secret_roots: Vec::new(),
        network_allowed: false,
        home: std::path::PathBuf::from("/tmp"),
        restrict_home: false,
        read_jail: false,
        git_branch: None,
        remote: None,
        session_id: None,
        process_supervisor: None,
        test_cache: None,
    }
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

#[tokio::test]
async fn v3_head_sections_are_byte_identical_across_follow_up_prompts() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
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
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "stable session goal".into(), None)
        .await
        .unwrap();
    assert_eq!(session.prompt_layout, 3);
    service.start(&session.id, "first prompt").await.unwrap();
    wait_until_idle(&engine, &session).await;
    service.start(&session.id, "second prompt").await.unwrap();
    wait_until_idle(&engine, &session).await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let first = &captured[0];
    let second = &captured[1];
    assert_eq!(request_head_text(first), request_head_text(second));
    assert_eq!(request_tail_text(first), "");
    assert_eq!(request_tail_text(second), "");
    for request in [first, second] {
        let head = request_head_text(request);
        assert!(head.contains("Session goal: 'stable session goal'"));
        assert!(!head.contains("Objective: first prompt"));
        assert!(!head.contains("Objective: second prompt"));
        assert!(!head.contains("first prompt"));
        assert!(!head.contains("second prompt"));
        assert!(!head.contains("Task objective"));
    }
    assert_eq!(first.messages.last().unwrap().content, "first prompt");
    assert_eq!(second.messages.last().unwrap().content, "second prompt");
    assert!(
        second
            .messages
            .iter()
            .any(|message| message.content == "first prompt")
    );
    assert!(
        second
            .messages
            .iter()
            .any(|message| message.content == "first answer")
    );

    let prefixes = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 10)
        .await
        .unwrap();
    assert_eq!(prefixes.len(), 2);
    let first_prefix = &prefixes[0].payload;
    let second_prefix = &prefixes[1].payload;
    assert_eq!(first_prefix["prompt_layout"], 3);
    assert_eq!(first_prefix["prefix_hash"], second_prefix["prefix_hash"]);
    assert_eq!(
        first_prefix["stable_prefix_hash"],
        second_prefix["stable_prefix_hash"]
    );
    let head_keys = first_prefix["head_sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section["key"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(head_keys[0], "core/charter");
    assert!(head_keys.contains(&"session/context".to_string()));
    assert!(!head_keys.contains(&"run/task".to_string()));
    assert!(first_prefix["tail_sections"].as_array().unwrap().is_empty());
    assert!(!first_prefix.to_string().contains("first prompt"));
    assert!(!first_prefix.to_string().contains("stable session goal"));

    let instructions = engine
        .session_instructions(&session.id, None)
        .await
        .unwrap();
    assert_eq!(instructions.prompt_layout, 3);
    assert_eq!(instructions.states.len(), 1);
    let leader = &instructions.states[0];
    assert_eq!(leader.role.as_deref(), Some("leader"));
    assert_eq!(leader.sections.len(), head_keys.len());
    assert!(leader.sections.iter().all(|section| !section.tail));
    assert!(leader.sections.iter().all(|section| section.available));
    // The RPC exposes hashes and sizes only.
    let encoded = serde_json::to_string(&instructions).unwrap();
    assert!(!encoded.contains("stable session goal"));
    assert!(!encoded.contains("You are the Leader"));
}

#[tokio::test]
async fn v3_memory_is_tail_only_and_never_moves_the_head_prefix() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("before memory".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("after memory".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "memory isolation".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "first run").await.unwrap();
    wait_until_idle(&engine, &session).await;
    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "Memory brief marker",
            "The project uses the internal artifact registry for release notes.",
            vec!["release".into()],
        )
        .await
        .unwrap();
    service.start(&session.id, "second run").await.unwrap();
    wait_until_idle(&engine, &session).await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    let first = &captured[0];
    let second = &captured[1];
    assert_eq!(request_head_text(first), request_head_text(second));
    assert_eq!(request_tail_text(first), "");
    assert!(request_tail_text(second).contains("tail/memory:"));
    assert!(request_tail_text(second).contains("internal artifact registry"));
    assert!(!request_head_text(second).contains("internal artifact registry"));

    let prefixes = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 10)
        .await
        .unwrap();
    let first_prefix = &prefixes[0].payload;
    let second_prefix = &prefixes[1].payload;
    assert_eq!(first_prefix["prefix_hash"], second_prefix["prefix_hash"]);
    assert_eq!(
        first_prefix["head_sections"],
        second_prefix["head_sections"]
    );
    assert_ne!(
        first_prefix["volatile_hash"],
        second_prefix["volatile_hash"]
    );
    assert_eq!(
        second_prefix["tail_sections"][0]["key"].as_str(),
        Some("tail/memory")
    );
    assert!(first_prefix["tail_sections"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn legacy_v2_sessions_keep_the_combined_system_prompt() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("legacy answer".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "legacy layout".into(), None)
        .await
        .unwrap();
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    service.start(&session.id, "legacy prompt").await.unwrap();
    wait_until_idle(&engine, &session).await;

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    let request = &captured[0];
    assert!(
        request
            .system_sections
            .iter()
            .any(|section| section.kind == "volatile"
                && section.text.contains("Objective: legacy prompt"))
    );
    assert!(
        request
            .system_sections
            .iter()
            .all(|section| section.kind != "instruction")
    );
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty()
    );
    let prefix = engine
        .recent_events_of_kind(&session.id, "context.prompt_prefix", 1)
        .await
        .unwrap();
    assert_eq!(prefix[0].payload["prompt_layout"], 2);
}

#[tokio::test]
async fn instruction_state_replays_and_repairs_from_events() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![vec![
            StreamEvent::TextDelta("done".into()),
            StreamEvent::Done,
        ]])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "replay instructions".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "replay prompt").await.unwrap();
    wait_until_idle(&engine, &session).await;
    let before = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    assert!(!before.is_empty());
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);

    // Simulate a projection loss; replay must detect drift and repair it from
    // the `context.instruction` event without regenerating prompt text.
    let instruction_events = engine
        .recent_events_of_kind(&session.id, "context.instruction", 10)
        .await
        .unwrap();
    assert!(!instruction_events.is_empty());
    assert!(
        !instruction_events[0].payload["sections"]
            .as_array()
            .unwrap()
            .is_empty(),
        "instruction state event must carry the ordered sections: {}",
        instruction_events[0].payload
    );
    let all_events = engine
        .list_events(session.id.clone(), 0, 1_000)
        .await
        .unwrap();
    assert!(
        crate::instruction::states_from_events(&all_events).is_some(),
        "instruction event payload must replay"
    );
    engine
        .store()
        .replace_session_instructions(&session.id, &[])
        .unwrap();
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty()
    );
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
    assert_eq!(before.len(), after.len());
    assert_eq!(
        before.iter().map(|row| &row.hash).collect::<Vec<_>>(),
        after.iter().map(|row| &row.hash).collect::<Vec<_>>()
    );
}

#[test]
fn v3_session_context_uses_the_session_goal_and_run_task_is_task_scoped() {
    let project = vp_protocol::types::Project {
        id: "prj-1".into(),
        name: "Demo".into(),
        root: "/tmp/demo".into(),
        default_branch: "main".into(),
        created_at: "2026-11-12T00:00:00Z".into(),
    };
    let session = vp_protocol::types::Session {
        id: "ses-1".into(),
        project_id: project.id.clone(),
        goal: "ship the stable prefix".into(),
        status: "idle".into(),
        priority: 0,
        worktree: Some("/tmp/demo-wt".into()),
        branch: Some("session/demo".into()),
        token_budget: None,
        cost_budget_usd: None,
        time_budget_secs: None,
        unlimited: false,
        cost_spent_usd: 0.0,
        base_commit: None,
        tokens_in: 0,
        tokens_out: 0,
        prompt_layout: 3,
        created_at: "2026-11-12T00:00:00Z".into(),
    };
    let leader = help_spec(AgentRole::Leader, AgentProfile::Generic);
    let leader_parts = role_prompt_parts(
        AgentRole::Leader,
        &project,
        &session,
        &leader,
        "current follow-up",
    );
    assert!(
        leader_parts
            .session_context
            .contains("Session goal: 'ship the stable prefix'")
    );
    assert!(!leader_parts.session_context.contains("current follow-up"));
    assert!(leader_parts.run_task.is_none());

    let worker = help_spec(AgentRole::Worker, AgentProfile::Coding);
    let worker_parts = role_prompt_parts(
        AgentRole::Worker,
        &project,
        &session,
        &worker,
        "fix the parser",
    );
    assert!(
        worker_parts
            .session_context
            .contains("ship the stable prefix")
    );
    assert!(!worker_parts.session_context.contains("fix the parser"));
    assert_eq!(
        worker_parts.run_task.as_deref(),
        Some(
            "Task objective (stable for this run; do not treat it as a project convention):\nfix the parser"
        )
    );

    let manager = help_spec(AgentRole::Manager, AgentProfile::Generic);
    let manager_parts = role_prompt_parts(
        AgentRole::Manager,
        &project,
        &session,
        &manager,
        "department goal",
    );
    assert!(manager_parts.run_task.is_none());
}

#[tokio::test]
async fn missing_instruction_blob_is_rebuilt_and_diagnosed_without_moving_head_bytes() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![StreamEvent::TextDelta("first".into()), StreamEvent::Done],
            vec![StreamEvent::TextDelta("second".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "blob repair".into(), None)
        .await
        .unwrap();
    service.start(&session.id, "first run").await.unwrap();
    wait_until_idle(&engine, &session).await;
    let rows = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    let charter = rows
        .iter()
        .find(|row| row.key == "core/charter")
        .expect("leader core charter")
        .clone();
    assert!(
        engine
            .store()
            .delete_instruction_blob(&charter.hash)
            .unwrap()
    );
    service.start(&session.id, "second run").await.unwrap();
    wait_until_idle(&engine, &session).await;

    let rebuilt = engine
        .recent_events_of_kind(&session.id, "context.instruction.rebuilt", 10)
        .await
        .unwrap();
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt[0].payload["key"], "core/charter");
    assert_eq!(rebuilt[0].payload["reason"], "missing_blob");
    assert_eq!(rebuilt[0].payload["old_hash"], charter.hash);
    assert_eq!(rebuilt[0].payload["new_hash"], charter.hash);
    let restored = engine
        .store()
        .get_instruction_blob(&charter.hash)
        .unwrap()
        .unwrap();
    assert!(restored.text.contains("Role charter:"));

    let captured = requests.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert_eq!(
        request_head_text(&captured[0]),
        request_head_text(&captured[1])
    );
}

#[tokio::test]
async fn v3_warm_sends_head_only_and_preserves_the_verified_tail_state() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![StreamEvent::TextDelta("run".into()), StreamEvent::Done],
            vec![StreamEvent::TextDelta("warm".into()), StreamEvent::Done],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "warm v3".into(), None)
        .await
        .unwrap();
    engine
        .create_memory_as(
            &session.id,
            "leader",
            "Leader",
            "fact",
            "Warm memory marker",
            "Warm must not send this memory in the stable prefix.",
            vec![],
        )
        .await
        .unwrap();
    service.start(&session.id, "first run").await.unwrap();
    wait_until_idle(&engine, &session).await;
    let before = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    assert!(before.iter().any(|row| row.tail));

    let warm = service.warm_cache_now(&session.id, None).await.unwrap();
    assert!(warm.warmed);
    {
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(captured[1].tail_sections.is_empty());
        assert!(!request_head_text(&captured[1]).contains("Warm must not send"));
        assert!(
            !captured[1]
                .messages
                .iter()
                .any(|message| { message.content.contains("Warm must not send this memory") })
        );
    }
    let after = engine
        .store()
        .list_session_instructions(&session.id)
        .unwrap();
    assert!(after.iter().any(|row| row.tail));
    assert_eq!(
        before.iter().map(|row| &row.hash).collect::<Vec<_>>(),
        after.iter().map(|row| &row.hash).collect::<Vec<_>>()
    );
    let events = engine
        .recent_events_of_kind(&session.id, "context.instruction", 20)
        .await
        .unwrap();
    let latest = events.last().unwrap();
    let keys = latest.payload["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section["key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(keys.contains(&"tail/memory"));
}

#[test]
fn v3_task_agents_receive_run_task_and_isolated_instruction_state() {
    run_with_large_stack(|| async {
        let dir = TempDir::new().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
            requests: requests.clone(),
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
            .create_session(project.id, "scoped delegation".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service
            .start(&session.id, "scoped delegation")
            .await
            .unwrap();
        wait_until_idle(&engine, &session).await;

        let captured = requests.lock().unwrap().clone();
        assert!(captured.len() >= 4);
        let role_of = |request: &ChatRequest| {
            request
                .system_sections
                .iter()
                .find_map(|section| {
                    section
                        .text
                        .lines()
                        .find(|line| line.starts_with("Agent: "))
                        .map(str::to_string)
                })
                .unwrap_or_default()
        };
        let leader_request = captured
            .iter()
            .find(|request| role_of(request).ends_with("(leader)"))
            .expect("leader request");
        assert!(
            !leader_request
                .system_sections
                .iter()
                .any(|section| section.kind == "task")
        );
        assert!(
            !leader_request
                .system_sections
                .iter()
                .any(|section| section.text.contains("Task objective"))
        );
        for role in ["manager", "worker", "labour"] {
            let request = captured
                .iter()
                .find(|request| role_of(request).ends_with(&format!("({role})")))
                .unwrap_or_else(|| panic!("{role} request"));
            let task = request
                .system_sections
                .iter()
                .find(|section| section.kind == "task");
            if role == "manager" {
                assert!(task.is_none(), "Manager objective must stay volatile");
                assert!(!request_head_text(request).contains("Task objective"));
            } else {
                let task = task.unwrap_or_else(|| panic!("{role} run/task section"));
                assert!(task.text.contains("Task objective"));
                assert!(task.text.contains(if role == "worker" {
                    "inspect the readme"
                } else {
                    "read the readme"
                }));
            }
            assert!(request_head_text(request).contains("Session goal: 'scoped delegation'"));
        }

        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents
            .iter()
            .find(|agent| agent.role == "worker")
            .expect("worker agent");
        let worker_scope = format!("{}#{}", session.id, worker.id);
        let worker_state = engine
            .store()
            .list_session_instructions(&worker_scope)
            .unwrap();
        assert!(worker_state.iter().any(|row| row.key == "run/task"));
        let leader_state = engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap();
        assert!(!leader_state.iter().any(|row| row.key == "run/task"));
        let report = engine
            .session_instructions(&session.id, Some(&worker.id))
            .await
            .unwrap();
        assert_eq!(report.states.len(), 1);
        assert_eq!(
            report.states[0].agent_id.as_deref(),
            Some(worker.id.as_str())
        );
        assert_eq!(report.states[0].role.as_deref(), Some("worker"));
        assert!(
            report.states[0]
                .sections
                .iter()
                .any(|section| section.key == "run/task" && !section.tail)
        );
    });
}

async fn run_one_cached_leader(
    _dir: &TempDir,
    service: &AgentService,
    engine: &Engine,
    project_id: &str,
    goal: &str,
    prompt: &str,
) -> vp_protocol::types::Session {
    let session = engine
        .create_session(project_id.to_string(), goal.to_string(), None)
        .await
        .unwrap();
    service.start(&session.id, prompt).await.unwrap();
    let _ = wait_until_idle(engine, &session).await;
    session
}

fn cached_usage_provider(read: u32) -> Arc<dyn Provider> {
    let response = vec![
        StreamEvent::TextDelta("cached answer".into()),
        StreamEvent::Usage {
            input_tokens: 1_000,
            output_tokens: 10,
            cache_read_tokens: read,
            cache_write_tokens: 0,
        },
        StreamEvent::Done,
    ];
    Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            response.clone(),
            response.clone(),
            response.clone(),
            response.clone(),
            response,
        ])),
    })
}

#[tokio::test]
async fn cache_epoch_is_stable_across_prompt_and_session_goal_changes() {
    let dir = TempDir::new().unwrap();
    let provider = cached_usage_provider(900);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let first = run_one_cached_leader(
        &dir,
        &service,
        &engine,
        &project.id,
        "first goal",
        "first prompt",
    )
    .await;
    let second = run_one_cached_leader(
        &dir,
        &service,
        &engine,
        &project.id,
        "second goal",
        "second prompt",
    )
    .await;

    let prefix = |session_id: String| {
        let engine = engine.clone();
        async move {
            engine
                .list_events(session_id, 0, 300)
                .await
                .unwrap()
                .into_iter()
                .find(|event| event.kind == "context.prompt_prefix")
                .expect("prompt prefix")
        }
    };
    let first_prefix = prefix(first.id).await;
    let second_prefix = prefix(second.id).await;
    assert_eq!(first_prefix.payload["cache_epoch_source"], "l4");
    assert_ne!(
        first_prefix.payload["prefix_hash"], second_prefix.payload["prefix_hash"],
        "session context changes the full head prefix"
    );
    assert_eq!(
        first_prefix.payload["cache_epoch"], second_prefix.payload["cache_epoch"],
        "the shareable epoch excludes prompt/session/run/tail bytes"
    );
    assert_eq!(
        first_prefix.payload["cache_epoch_model"],
        second_prefix.payload["cache_epoch_model"]
    );
    assert_eq!(
        first_prefix.payload["cache_epoch_tools_hash"],
        second_prefix.payload["cache_epoch_tools_hash"]
    );
    assert!(
        first_prefix.payload["cache_epoch_core_sections"]
            .as_array()
            .is_some_and(|sections| !sections.is_empty())
    );
    assert!(
        first_prefix.payload["cache_epoch_project_sections"]
            .as_array()
            .is_some()
    );
    assert_eq!(first_prefix.payload["cache_epoch_change_reason"], "first");
    assert_eq!(second_prefix.payload["cache_epoch_change_reason"], "first");
}

#[tokio::test]
async fn new_session_reports_cross_session_reuse_from_provider_usage() {
    let dir = TempDir::new().unwrap();
    let provider = cached_usage_provider(900);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let first = run_one_cached_leader(&dir, &service, &engine, &project.id, "one", "one").await;
    let first_events = engine.list_events(first.id.clone(), 0, 300).await.unwrap();
    let first_cache = first_events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("first cache event");
    assert_eq!(first_cache.payload["cache_epoch_reused_from_sessions"], 0);
    assert_eq!(first_cache.payload["cross_session_first_call"], true);
    assert_eq!(first_cache.payload["cross_session_cache_read_tokens"], 0);

    let second = run_one_cached_leader(&dir, &service, &engine, &project.id, "two", "two").await;
    let second_events = engine.list_events(second.id.clone(), 0, 300).await.unwrap();
    let second_cache = second_events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("second cache event");
    assert_eq!(second_cache.payload["cache_epoch_reused_from_sessions"], 1);
    assert_eq!(second_cache.payload["cross_session_first_call"], true);
    assert_eq!(
        second_cache.payload["cross_session_cache_read_tokens"], 900,
        "provider-reported reads are the only reuse evidence"
    );
    assert_eq!(second_cache.payload["cache_epoch_session_count"], 2);
    assert_eq!(second_cache.payload["miss_reason"], serde_json::Value::Null);

    let status = engine.session_cache_status(&second.id).await.unwrap();
    assert_eq!(status.prefix_reused_from_sessions, 1);
    assert_eq!(status.cross_session_hit_tokens, 900);
    assert_eq!(status.cross_session_first_calls, 1);
    assert_eq!(status.cache_epoch_session_count, 2);
    assert_eq!(status.cache_epoch_call_count, 2);
    assert!(status.cache_epoch.is_some());
    assert_eq!(status.cache_epoch_source.as_deref(), Some("l4"));

    let report = engine
        .session_cache_epoch(&second.id, Some(10))
        .await
        .unwrap();
    assert_eq!(report.project_id, project.id);
    assert_eq!(report.total_epochs, 1);
    assert_eq!(report.epochs.len(), 1);
    assert_eq!(report.epochs[0].session_count, 2);
    assert_eq!(report.epochs[0].reused_from_sessions, 1);
    assert_eq!(report.prefix_reused_from_sessions, 1);
    assert_eq!(report.cross_session_hit_tokens, 900);
    assert_eq!(
        report.timeline.len(),
        1,
        "the timeline is scoped to this session"
    );
    assert!(report.timeline.iter().all(|entry| entry.kind == "prefix"));
    assert_eq!(
        report
            .current_epoch
            .as_ref()
            .map(|row| row.cache_epoch.as_str()),
        status.cache_epoch.as_deref()
    );
    // The epoch RPC is read-only and bounded.
    let bounded = engine
        .session_cache_epoch(&second.id, Some(1))
        .await
        .unwrap();
    assert_eq!(bounded.timeline.len(), 1);
    assert_eq!(bounded.epochs.len(), 1);
    assert!(!bounded.timeline_truncated);
}

#[tokio::test]
async fn existing_epoch_with_zero_provider_reads_is_a_provider_cache_miss() {
    let dir = TempDir::new().unwrap();
    let provider = cached_usage_provider(0);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let first = run_one_cached_leader(&dir, &service, &engine, &project.id, "one", "one").await;
    let first_events = engine.list_events(first.id.clone(), 0, 300).await.unwrap();
    let first_cache = first_events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("first cache event");
    assert_eq!(first_cache.payload["miss_reason"], "first_call");

    let second = run_one_cached_leader(&dir, &service, &engine, &project.id, "two", "two").await;
    let second_events = engine.list_events(second.id.clone(), 0, 300).await.unwrap();
    let second_cache = second_events
        .iter()
        .find(|event| event.kind == "context.cache")
        .expect("second cache event");
    assert_eq!(second_cache.payload["miss_reason"], "provider_cache_miss");
    assert_eq!(second_cache.payload["cross_session_first_call"], true);
    assert_eq!(second_cache.payload["cross_session_cache_read_tokens"], 0);
    let status = engine.session_cache_status(&second.id).await.unwrap();
    assert_eq!(status.provider_cache_misses, 1);
    assert_eq!(status.prefix_changes, 0);
    assert_eq!(status.cross_session_hit_tokens, 0);
}

#[tokio::test]
async fn compaction_records_epoch_reset_and_lost_cache_read_estimate() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("run".into()),
                StreamEvent::Usage {
                    input_tokens: 1_000,
                    output_tokens: 10,
                    cache_read_tokens: 900,
                    cache_write_tokens: 0,
                },
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("cache-aware summary".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_leader_prices(&service, 3.0, 15.0, 0.3, 3.75);
    set_compaction(&service, 0, 3);
    let session = run_one_cached_leader(&dir, &service, &engine, &project.id, "one", "one").await;
    let before = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("prompt prefix");
    let epoch = before.payload["cache_epoch"].as_str().unwrap().to_string();

    for index in 0..14 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("message {index}: compaction cache epoch boundary {index}"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    let result = service.compact_now(&session.id).await.unwrap();
    assert!(result.compacted);
    let summary = result.compaction.expect("summary");
    assert_eq!(summary.cache_epoch_before.as_deref(), Some(epoch.as_str()));
    assert_eq!(summary.cache_epoch_after.as_deref(), Some(epoch.as_str()));
    assert!(!summary.cache_epoch_changed);
    assert!(!summary.cache_reset_reason.is_none());
    assert!(summary.estimated_lost_cache_read_usd >= 0.0);

    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let reset = events
        .iter()
        .find(|event| event.kind == "context.compaction.cache_reset")
        .expect("cache reset event");
    assert_eq!(reset.payload["reset_reason"], "compaction");
    assert_eq!(reset.payload["cache_epoch_before"], epoch);
    assert_eq!(reset.payload["cache_epoch_after"], epoch);
    assert_eq!(reset.payload["cache_epoch_changed"], false);
    assert!(reset.payload["invalidated_tokens"].as_u64().unwrap() > 0);
    assert!(
        reset.payload["estimated_lost_cache_read_usd"]
            .as_f64()
            .unwrap()
            >= 0.0
    );
    assert!(
        reset.payload.get("summary").is_none(),
        "the reset event is bounded accounting, not a transcript"
    );

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.compaction_cache_resets, 1);
    assert_eq!(status.compaction_resets, 1);
    assert!(status.compaction_lost_cache_usd >= 0.0);
    assert_eq!(status.cache_epoch.as_deref(), Some(epoch.as_str()));

    let report = engine
        .session_cache_epoch(&session.id, Some(50))
        .await
        .unwrap();
    assert_eq!(report.compaction_resets, 1);
    assert!(report.timeline.iter().any(|entry| {
        entry.kind == "compaction_reset"
            && entry.change_reason.as_deref() == Some("compaction")
            && entry.compaction_count == Some(1)
    }));
    assert_eq!(
        report
            .current_epoch
            .as_ref()
            .map(|row| row.cache_epoch.as_str()),
        Some(epoch.as_str()),
        "compaction does not rewrite the shareable epoch"
    );

    // Compaction rewrites the active context only. Original messages remain
    // stored for search/audit, and the epoch projection is untouched.
    let stored = engine.list_messages(&session.id).await.unwrap();
    assert!(
        stored.iter().any(|message| {
            message
                .content
                .contains("message 0: compaction cache epoch boundary")
        }),
        "pre-compaction history must remain stored"
    );
    let observations = engine
        .store()
        .list_prompt_prefix_observations(&project.id, &session.id)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].calls, 1);
    assert_eq!(observations[0].cache_epoch, epoch);
}

#[tokio::test]
async fn warm_records_its_own_usage_without_changing_the_epoch_or_observation() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("run".into()),
                StreamEvent::Usage {
                    input_tokens: 1_000,
                    output_tokens: 10,
                    cache_read_tokens: 900,
                    cache_write_tokens: 0,
                },
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("OK".into()),
                StreamEvent::Usage {
                    input_tokens: 700,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 600,
                },
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_leader_prices(&service, 3.0, 15.0, 0.3, 3.75);
    let session = run_one_cached_leader(&dir, &service, &engine, &project.id, "one", "one").await;
    let before = engine.session_cache_status(&session.id).await.unwrap();
    let epoch = before.cache_epoch.clone().expect("epoch");
    let calls = before.cache_epoch_call_count;
    let warm = service.warm_cache_now(&session.id, None).await.unwrap();
    assert!(warm.warmed);
    assert_eq!(warm.cache_epoch.as_deref(), Some(epoch.as_str()));
    assert!(warm.cache_epoch_unchanged);
    assert!(warm.input_tokens > 0);
    assert!(warm.cost_usd >= 0.0);
    let after = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(after.cache_epoch.as_deref(), Some(epoch.as_str()));
    assert_eq!(
        after.cache_epoch_call_count, calls,
        "warm calls are billed but are not epoch observations"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    let warm_event = events
        .iter()
        .filter(|event| event.kind == "context.cache")
        .find(|event| event.payload["warm"] == true)
        .expect("warm cache event");
    assert_eq!(warm_event.payload["cache_epoch"], epoch);
    assert_eq!(warm_event.payload["cache_epoch_unchanged"], true);
    assert_eq!(warm_event.payload["cross_session_cache_read_tokens"], 0);
}

#[tokio::test]
async fn normal_run_makes_one_provider_call_and_never_warm_caches_implicitly() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("one call".into()),
                StreamEvent::Usage {
                    input_tokens: 1_000,
                    output_tokens: 10,
                    cache_read_tokens: 900,
                    cache_write_tokens: 0,
                },
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("unexpected second call".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = run_one_cached_leader(&dir, &service, &engine, &project.id, "one", "one").await;
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "a normal run must not warm or probe the provider"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "context.cache_warm"),
        "warm is explicit/opt-in only"
    );
    assert!(
        !events
            .iter()
            .any(|event| { event.kind == "context.cache" && event.payload["warm"] == true }),
        "no implicit warm cache event"
    );
}

#[tokio::test]
async fn automatic_compaction_emits_one_cache_reset_and_preserves_the_epoch() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::TextDelta("automatic summary".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("answer after automatic compaction".into()),
                StreamEvent::Usage {
                    input_tokens: 2_000,
                    output_tokens: 10,
                    cache_read_tokens: 1_500,
                    cache_write_tokens: 0,
                },
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    set_compaction(&service, 1, 3);
    set_leader_prices(&service, 3.0, 15.0, 0.3, 3.75);
    let session = engine
        .create_session(project.id.clone(), "auto compaction".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    for index in 0..14 {
        engine
            .append_message(
                &session.id,
                if index % 2 == 0 { "user" } else { "assistant" },
                &format!("automatic history {index}: enough detail to force a late boundary"),
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
    }
    service
        .start(&session.id, "continue after history")
        .await
        .unwrap();
    let _ = wait_until_idle(&engine, &session).await;

    let events = engine
        .list_events(session.id.clone(), 0, 600)
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
    let resets = events
        .iter()
        .filter(|event| event.kind == "context.compaction.cache_reset")
        .collect::<Vec<_>>();
    assert_eq!(
        resets.len(),
        1,
        "one automatic boundary must produce one reset event"
    );
    assert_eq!(resets[0].payload["reset_reason"], "compaction");
    assert_eq!(resets[0].payload["cache_epoch_before"], epoch);
    assert_eq!(resets[0].payload["cache_epoch_after"], epoch);
    assert_eq!(resets[0].payload["cache_epoch_changed"], false);
    assert!(resets[0].payload["invalidated_tokens"].as_u64().unwrap() > 0);
    assert!(events.iter().any(|event| event.kind == "agent.message"
        && event.payload["text"] == "answer after automatic compaction"));
    let stored = engine.list_messages(&session.id).await.unwrap();
    assert!(
        stored
            .iter()
            .any(|message| message.content.contains("automatic history 0")),
        "automatic compaction must not delete stored history"
    );
    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.cache_epoch.as_deref(), Some(epoch.as_str()));
    assert_eq!(status.compaction_cache_resets, 1);
    let report = engine
        .session_cache_epoch(&session.id, Some(50))
        .await
        .unwrap();
    assert_eq!(report.compaction_resets, 1);
    assert!(
        report
            .timeline
            .iter()
            .any(|entry| entry.kind == "compaction_reset")
    );
}

#[tokio::test]
async fn model_switch_creates_a_second_project_epoch_and_names_the_change() {
    let dir = TempDir::new().unwrap();
    let provider = cached_usage_provider(900);
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let first = run_one_cached_leader(&dir, &service, &engine, &project.id, "one", "one").await;
    let first_prefix = engine
        .list_events(first.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("first prefix");
    let first_epoch = first_prefix.payload["cache_epoch"]
        .as_str()
        .unwrap()
        .to_string();

    {
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.role_models.leader.model = Some("second-model".into());
        *service.config.write().unwrap() = Some(config);
    }
    let second = run_one_cached_leader(&dir, &service, &engine, &project.id, "two", "two").await;
    let second_prefix = engine
        .list_events(second.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.prompt_prefix")
        .expect("second prefix");
    let second_epoch = second_prefix.payload["cache_epoch"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first_epoch, second_epoch);
    assert_eq!(second_prefix.payload["cache_epoch_model"], "second-model");
    assert_eq!(
        second_prefix.payload["cache_epoch_core_hash"],
        first_prefix.payload["cache_epoch_core_hash"],
        "the model switch does not rewrite the instruction bytes"
    );

    let cache = engine
        .list_events(second.id.clone(), 0, 300)
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.kind == "context.cache")
        .expect("cache event");
    assert_eq!(cache.payload["cache_epoch"], second_epoch);
    assert_eq!(cache.payload["cache_epoch_reused_from_sessions"], 0);
    let report = engine
        .project_cache_epochs(&project.id, Some(10))
        .await
        .unwrap();
    assert_eq!(report.total_epochs, 2);
    assert_eq!(report.total_sessions, 2);
    assert!(
        report
            .epochs
            .iter()
            .any(|epoch| epoch.cache_epoch == first_epoch)
    );
    assert!(
        report
            .epochs
            .iter()
            .any(|epoch| epoch.cache_epoch == second_epoch)
    );
}

#[tokio::test]
async fn workspace_write_creates_a_done_receipt_and_replays_without_rewriting() {
    let dir = TempDir::new().unwrap();
    let write_call = || {
        vec![
            StreamEvent::ToolCall(ToolCall {
                id: format!("call_write_{}", Uuid::now_v7().simple()),
                name: "write_file".into(),
                arguments: serde_json::json!({
                    "path": "notes/effect.txt",
                    "content": "effect receipt\n"
                })
                .to_string(),
            }),
            StreamEvent::Done,
        ]
    };
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            write_call(),
            vec![
                StreamEvent::TextDelta("wrote the file".into()),
                StreamEvent::Done,
            ],
            write_call(),
            vec![
                StreamEvent::TextDelta("the write was replayed".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider);
    let session = engine
        .create_session(project.id, "effect replay".into(), None)
        .await
        .unwrap();
    service
        .start(&session.id, "write the file once")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    let worktree = std::path::PathBuf::from(session.worktree.clone().unwrap());
    let path = worktree.join("notes/effect.txt");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "effect receipt\n");

    let effects = engine
        .session_effects(&session.id, None, true, 100)
        .await
        .unwrap();
    assert_eq!(effects.counts.total, 1, "{effects:?}");
    assert_eq!(effects.counts.done, 1);
    let receipt = &effects.receipts[0];
    assert_eq!(receipt.target.key, "notes/effect.txt");
    assert_eq!(
        receipt.evidence.sha256.as_deref(),
        Some(crate::effect::sha256_bytes(b"effect receipt\n").as_str())
    );

    // Remove the file behind the runtime's back. A replayed done receipt must
    // not execute the write again, so the file stays absent.
    std::fs::remove_file(&path).unwrap();
    service
        .start(&session.id, "try the same write again")
        .await
        .unwrap();
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    assert!(!path.exists(), "a done receipt must not rewrite the file");
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "write_file"
            && event.payload["output"]
                .as_str()
                .is_some_and(|text| text.contains("effect already completed"))
    }));
    let effects = engine
        .session_effects(&session.id, None, true, 100)
        .await
        .unwrap();
    assert_eq!(
        effects.counts.total, 1,
        "replay must not create a second row"
    );
}

#[tokio::test]
async fn wait_for_gate_tool_defers_and_resumes_through_continuation() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id, "gate wait".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let gate = engine
        .open_gate(vp_protocol::types::GateOpenParams {
            session_id: session.id.clone(),
            task_id: None,
            kind: Some("ci".into()),
            label: Some("unit tests".into()),
            payload: None,
            expires_at: None,
            after_secs: None,
        })
        .await
        .unwrap();
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_gate".into(),
            name: "wait_for_gate".into(),
            arguments: serde_json::json!({"gate_id": gate.gate.id}).to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::TextDelta("resumed after the gate".into()),
        StreamEvent::Done,
    ]);
    let config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service configured")
        .clone();
    let control = Arc::new(RunControl::new());
    run_agent(
        engine.clone(),
        config,
        &session.id,
        "wait for CI",
        control,
        None,
    )
    .await
    .unwrap();

    assert_eq!(requests.lock().unwrap().len(), 1, "no second provider call");
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    let current = sessions
        .into_iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert_eq!(current.status, "idle");
    let continuations = engine
        .session_continuations(&session.id, Some("pending"), Some(10))
        .await
        .unwrap();
    assert_eq!(continuations.continuations.len(), 1);
    assert_eq!(
        continuations.continuations[0].watch_gate_id.as_deref(),
        Some(gate.gate.id.as_str())
    );
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "agent.deferred"));
    assert!(
        !events
            .iter()
            .any(|event| event.payload.to_string().contains(gate.token.expose()))
    );

    engine
        .resolve_gate(
            &gate.gate.id,
            gate.token.expose(),
            Some("resolved"),
            Some("green"),
            Some("ci"),
        )
        .await
        .unwrap();
    assert_eq!(service.reconcile_session_continuations().await.unwrap(), 1);
    let sessions = wait_until_idle(&engine, &session).await;
    assert_eq!(sessions.status, "idle");
    assert_eq!(requests.lock().unwrap().len(), 2);
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );
    assert!(events.iter().any(|event| {
        event.kind == "agent.message"
            && event.payload["text"]
                .as_str()
                .is_some_and(|text| text.contains("resumed after the gate"))
    }));
}

#[tokio::test]
async fn wait_for_gate_rejects_an_unknown_gate_without_creating_a_continuation() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id, "unknown gate".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_gate".into(),
            name: "wait_for_gate".into(),
            arguments: serde_json::json!({"gate_id": "missing-gate"}).to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::TextDelta("continued after the typed error".into()),
        StreamEvent::Done,
    ]);
    let config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service configured")
        .clone();
    run_agent(
        engine.clone(),
        config,
        &session.id,
        "try a bad gate",
        Arc::new(RunControl::new()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
    let continuations = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert!(continuations.continuations.is_empty());
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "tool.finished"
            && event.payload["name"] == "wait_for_gate"
            && event.payload["error_code"] == "GATE_NOT_FOUND"
    }));
}

#[tokio::test]
async fn worker_wait_for_gate_releases_the_worker_slot() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id.clone(), "worker gate".into(), None)
        .await
        .unwrap();
    let gate = engine
        .open_gate(vp_protocol::types::GateOpenParams {
            session_id: session.id.clone(),
            task_id: None,
            kind: Some("deploy".into()),
            label: Some("staging".into()),
            payload: None,
            expires_at: None,
            after_secs: None,
        })
        .await
        .unwrap();
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_gate".into(),
            name: "wait_for_gate".into(),
            arguments: serde_json::json!({"gate_id": gate.gate.id}).to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::TextDelta("unexpected second call".into()),
        StreamEvent::Done,
    ]);
    let session_record = engine.store().get_session(&session.id).unwrap().unwrap();
    let project_record = engine.store().get_project(&project.id).unwrap().unwrap();
    let worktree = PathBuf::from(session_record.worktree.clone().unwrap());
    let spec = AgentSpec {
        id: "worker-1".into(),
        role: AgentRole::Worker,
        name: "Worker".into(),
        depth: 1,
        parent_name: Some("Leader".into()),
        title: None,
        task_id: None,
        department: None,
        identity_id: None,
        profile: AgentProfile::Coding,
        allow_delegation: false,
        task_kind: None,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: session_record.branch.clone().unwrap_or_default(),
        session_worktree: worktree.clone(),
        session_branch: session_record.branch.clone().unwrap_or_default(),
        write_scope: Vec::new(),
        base_commit: session_record.base_commit.clone().unwrap_or_default(),

        workspace_id: None,
        session_workspace_id: None,
        parent_workspace_id: None,
    };
    let config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service configured")
        .clone();
    let budget = SessionBudget::new(
        None,
        0,
        AgentCaps {
            managers: 1,
            workers: 2,
            labour: 2,
        },
    );
    let control = RunControl::new();
    let mut conversation = Vec::new();
    let outcome = run_loop(
        &engine,
        &config,
        &session_record,
        &project_record,
        &control,
        &budget,
        &spec,
        "wait for deploy",
        &mut conversation,
        None,
        false,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, LoopOutcome::Deferred(_)));
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(
        budget.state.lock().unwrap().workers,
        0,
        "a waiting Worker must not retain its slot"
    );
    let continuations = engine
        .session_continuations(&session.id, Some("pending"), Some(10))
        .await
        .unwrap();
    assert_eq!(continuations.continuations.len(), 1);
}

#[tokio::test]
async fn worker_wait_inside_delegation_ends_the_chain_and_keeps_the_session_idle() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id, "nested gate wait".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let gate = engine
        .open_gate(vp_protocol::types::GateOpenParams {
            session_id: session.id.clone(),
            task_id: None,
            kind: Some("ci".into()),
            label: Some("nested tests".into()),
            payload: None,
            expires_at: None,
            after_secs: None,
        })
        .await
        .unwrap();
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_manager".into(),
            name: "delegate".into(),
            arguments: serde_json::json!({
                "role": "manager",
                "title": "Implementation",
                "objective": "coordinate the wait"
            })
            .to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_worker".into(),
            name: "delegate".into(),
            arguments: serde_json::json!({
                "role": "worker",
                "objective": "wait for the CI gate",
                "write_scope": []
            })
            .to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_gate".into(),
            name: "wait_for_gate".into(),
            arguments: serde_json::json!({"gate_id": gate.gate.id}).to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::TextDelta("manager saw the wait".into()),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::TextDelta("leader saw the manager".into()),
        StreamEvent::Done,
    ]);
    let config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service configured")
        .clone();
    run_agent(
        engine.clone(),
        config,
        &session.id,
        "delegate a gate wait",
        Arc::new(RunControl::new()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 5);
    let current = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert_eq!(current.status, "idle");
    let continuations = engine
        .session_continuations(&session.id, Some("pending"), Some(10))
        .await
        .unwrap();
    assert_eq!(continuations.continuations.len(), 1);
    assert_eq!(
        continuations.continuations[0].watch_gate_id.as_deref(),
        Some(gate.gate.id.as_str())
    );
    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished" && event.payload["status"] == "waiting_gate"
    }));
    assert!(
        !events
            .iter()
            .any(|event| { event.payload.to_string().contains(gate.token.expose()) })
    );
}

#[tokio::test]
async fn manager_wait_propagates_to_the_top_level_and_stops_the_run() {
    let dir = TempDir::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(RecordingProvider {
        requests: requests.clone(),
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = test_service_with_provider(&dir, provider.clone());
    let session = engine
        .create_session(project.id, "manager gate".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    let gate = engine
        .open_gate(vp_protocol::types::GateOpenParams {
            session_id: session.id.clone(),
            task_id: None,
            kind: Some("human".into()),
            label: Some("design sign-off".into()),
            payload: None,
            expires_at: None,
            after_secs: None,
        })
        .await
        .unwrap();
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_manager".into(),
            name: "delegate".into(),
            arguments: serde_json::json!({
                "role": "manager",
                "title": "Design",
                "objective": "wait for sign-off"
            })
            .to_string(),
        }),
        StreamEvent::Done,
    ]);
    provider.responses.lock().unwrap().push_back(vec![
        StreamEvent::ToolCall(ToolCall {
            id: "call_gate".into(),
            name: "wait_for_gate".into(),
            arguments: serde_json::json!({"gate_id": gate.gate.id}).to_string(),
        }),
        StreamEvent::Done,
    ]);
    let config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service configured")
        .clone();
    run_agent(
        engine.clone(),
        config,
        &session.id,
        "delegate a manager wait",
        Arc::new(RunControl::new()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
    let current = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert_eq!(current.status, "idle");
    let events = engine
        .list_events(session.id.clone(), 0, 400)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "agent.deferred"));
    assert!(events.iter().any(|event| {
        event.kind == "agent.finished"
            && event.payload["agent_role"] == "manager"
            && event.payload["status"] == "waiting_gate"
    }));
    let continuations = engine
        .session_continuations(&session.id, Some("pending"), Some(10))
        .await
        .unwrap();
    assert_eq!(continuations.continuations.len(), 1);
}

#[tokio::test]
async fn role_contract_prompts_make_worker_the_technical_lead_and_labour_the_implementer() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    let session = engine
        .create_session(project.id.clone(), "role contract goal".into(), None)
        .await
        .unwrap();
    let worker = help_spec(AgentRole::Worker, AgentProfile::Coding);
    let worker_parts = role_prompt_parts(
        AgentRole::Worker,
        &project,
        &session,
        &worker,
        "implement auth validation",
    );
    assert!(worker_parts.core_charter.contains("You are a Worker"));
    assert!(worker_parts.core_charter.contains("technical lead"));
    assert!(
        worker_parts
            .core_charter
            .contains("delegate(role=\"labour\")")
    );
    assert!(worker_parts.core_charter.contains("low-level design"));
    assert!(worker_parts.core_charter.contains("review"));
    assert!(
        worker_parts
            .core_charter
            .contains("bulk implementation yourself")
    );

    let labour = help_spec(AgentRole::Labour, AgentProfile::Coding);
    let labour_parts = role_prompt_parts(
        AgentRole::Labour,
        &project,
        &session,
        &labour,
        "write the auth handler",
    );
    assert!(labour_parts.core_charter.contains("You are Labour"));
    assert!(labour_parts.core_charter.contains("implementation worker"));
    assert!(labour_parts.core_charter.contains("cannot delegate"));
    assert!(labour_parts.core_charter.contains("write code and tests"));

    let manager = help_spec(AgentRole::Manager, AgentProfile::Generic);
    let manager_parts = role_prompt_parts(
        AgentRole::Manager,
        &project,
        &session,
        &manager,
        "own the platform department",
    );
    assert!(manager_parts.core_charter.contains("department architect"));
    assert!(manager_parts.core_charter.contains("must not edit files"));
    assert!(manager_parts.core_charter.contains("bulk code"));
    assert!(manager_parts.core_charter.contains("delegate_many"));

    let leader = help_spec(AgentRole::Leader, AgentProfile::Coding);
    let leader_parts = role_prompt_parts(
        AgentRole::Leader,
        &project,
        &session,
        &leader,
        "own the project",
    );
    assert!(
        leader_parts
            .core_charter
            .contains("project and portfolio owner")
    );
    assert!(leader_parts.core_charter.contains("final result"));
    assert!(leader_parts.core_charter.contains("bulk implementation"));
    assert!(
        leader_parts
            .core_charter
            .contains("implementation belongs to Labour")
    );
}

#[tokio::test]
async fn delegate_schema_exposes_only_the_child_role_and_leaf_roles_get_none() {
    let dir = TempDir::new().unwrap();
    let (_engine, service, _project) = test_service(&dir);
    let config = service
        .config
        .read()
        .unwrap()
        .as_ref()
        .expect("test service configured")
        .clone();

    let worker_tools = tool_schemas(&config, AgentRole::Worker, AgentProfile::Coding, true);
    let worker_delegate = worker_tools
        .iter()
        .find(|tool| tool.name == "delegate")
        .expect("Worker delegate schema");
    assert_eq!(
        worker_delegate.parameters["properties"]["role"]["enum"],
        serde_json::json!(["labour"])
    );
    assert!(worker_delegate.description.contains("Labour"));
    assert!(worker_delegate.description.contains("technical lead"));

    let manager_tools = tool_schemas(&config, AgentRole::Manager, AgentProfile::Generic, true);
    let manager_delegate = manager_tools
        .iter()
        .find(|tool| tool.name == "delegate")
        .expect("Manager delegate schema");
    assert_eq!(
        manager_delegate.parameters["properties"]["role"]["enum"],
        serde_json::json!(["worker"])
    );
    assert!(manager_delegate.description.contains("technical leads"));

    let leader_tools = tool_schemas(&config, AgentRole::Leader, AgentProfile::Coding, true);
    let leader_delegate = leader_tools
        .iter()
        .find(|tool| tool.name == "delegate")
        .expect("Leader delegate schema");
    assert_eq!(
        leader_delegate.parameters["properties"]["role"]["enum"],
        serde_json::json!(["manager"])
    );

    for (role, profile) in [
        (AgentRole::Labour, AgentProfile::Coding),
        (AgentRole::Verifier, AgentProfile::Verification),
        (AgentRole::Scout, AgentProfile::Research),
    ] {
        let tools = tool_schemas(&config, role, profile, true);
        assert!(
            tools.iter().all(|tool| tool.name != "delegate"),
            "{} must not advertise delegate",
            role.as_str()
        );
        assert!(
            role_tool_surface(role, profile)
                .iter()
                .all(|name| name != "delegate"),
            "{} policy surface must not allow delegate",
            role.as_str()
        );
    }
    assert!(
        role_tool_surface(AgentRole::Worker, AgentProfile::Coding)
            .iter()
            .any(|name| name == "delegate")
    );
    assert_eq!(AgentRole::Worker.child_role(), Some(AgentRole::Labour));
    assert_eq!(AgentRole::Labour.child_role(), None);
    assert_eq!(AgentRole::Verifier.child_role(), None);
    assert_eq!(AgentRole::Scout.child_role(), None);
}

#[test]
fn identity_roles_are_separate_from_the_task_scoped_roles() {
    for role in ["leader", "manager", "worker"] {
        assert!(crate::identity_prompt::identity_role_applies(role));
    }
    for role in ["labour", "verifier", "scout"] {
        assert!(!crate::identity_prompt::identity_role_applies(role));
    }
    // The AgentRole enum itself is unchanged: Labour remains a task-scoped
    // execution role, not a project identity.
    assert_eq!(AgentRole::parse("labour"), Some(AgentRole::Labour));
    assert_eq!(AgentRole::parse("scout"), Some(AgentRole::Scout));
}

async fn enable_identity_policy(engine: &Engine, project_id: &str) {
    engine
        .set_agent_identity_policy(
            project_id,
            vp_protocol::types::AgentIdentityPolicy {
                enabled: true,
                ..vp_protocol::types::AgentIdentityPolicy::default()
            },
        )
        .await
        .unwrap();
}

async fn project_identity(
    engine: &Engine,
    project_id: &str,
    role: &str,
    title: &str,
    component: &str,
) -> String {
    engine
        .create_agent_identity(vp_protocol::types::CreateAgentIdentityParams {
            project_id: project_id.to_string(),
            role: role.to_string(),
            title: Some(title.to_string()),
            department: Some("Platform".to_string()),
            component: Some(component.to_string()),
            profile: Some("coding".to_string()),
            model: Some("test-model".to_string()),
            ..vp_protocol::types::CreateAgentIdentityParams::default()
        })
        .await
        .unwrap()
        .identity
        .id
}

async fn assign_identity(engine: &Engine, session_id: &str, identity_id: &str) {
    engine
        .assign_session_identity(vp_protocol::types::SessionAssignParams {
            session_id: session_id.to_string(),
            identity_id: identity_id.to_string(),
            ..vp_protocol::types::SessionAssignParams::default()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn assigned_manager_and_worker_get_identity_sections_but_task_roles_do_not() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    enable_identity_policy(&engine, &project.id).await;
    let manager = project_identity(
        &engine,
        &project.id,
        "manager",
        "Platform Manager",
        "platform",
    )
    .await;
    let worker = project_identity(&engine, &project.id, "worker", "Auth Tech Lead", "auth").await;
    let session = engine
        .create_session(project.id.clone(), "identity section goal".into(), None)
        .await
        .unwrap();
    assign_identity(&engine, &session.id, &manager).await;
    assign_identity(&engine, &session.id, &worker).await;

    let mut manager_spec = help_spec(AgentRole::Manager, AgentProfile::Generic);
    manager_spec.title = Some("Platform Manager".to_string());
    manager_spec.department = Some("Platform".to_string());
    let manager_sections = build_prompt_sections(
        &engine,
        &project,
        &session,
        &manager_spec,
        None,
        "own the platform department",
        true,
        InstructionBuildMode::Materialize,
        None,
    )
    .await
    .unwrap();
    let manager_identity = manager_sections
        .exact_text(crate::identity_prompt::IDENTITY_SECTION_KEY)
        .expect("manager identity section");
    assert!(manager_identity.contains("Role: manager"));
    assert!(manager_identity.contains("Title: Platform Manager"));
    assert!(manager_identity.contains("Component: platform"));
    assert!(manager_identity.contains("department architect"));
    assert_eq!(
        manager_sections
            .identity
            .as_ref()
            .map(|marker| marker.generation),
        Some(1)
    );

    let mut worker_spec = help_spec(AgentRole::Worker, AgentProfile::Coding);
    worker_spec.department = Some("Platform".to_string());
    let worker_sections = build_prompt_sections(
        &engine,
        &project,
        &session,
        &worker_spec,
        None,
        "implement auth",
        true,
        InstructionBuildMode::Materialize,
        None,
    )
    .await
    .unwrap();
    let worker_identity = worker_sections
        .exact_text(crate::identity_prompt::IDENTITY_SECTION_KEY)
        .expect("worker identity section");
    assert!(worker_identity.contains("Role: worker"));
    assert!(worker_identity.contains("Title: Auth Tech Lead"));
    assert!(worker_identity.contains("technical lead"));
    assert!(worker_identity.contains("Labour"));
    assert!(!worker_identity.contains("implement auth"));
    assert!(!worker_identity.contains("identity section goal"));
    let scoped = engine
        .session_instructions(&session.id, Some(&worker_spec.id))
        .await
        .unwrap();
    assert_eq!(scoped.states.len(), 1);
    assert_eq!(
        scoped.states[0].state_id,
        format!("{}#{}", session.id, worker_spec.id)
    );
    assert!(
        scoped.states[0]
            .sections
            .iter()
            .any(|section| section.key == "agent/identity" && !section.tail)
    );
    assert!(scoped.states[0].identity_hash.is_some());
    let encoded = serde_json::to_string(&scoped).unwrap();
    assert!(!encoded.contains("Auth Tech Lead"));
    assert!(!encoded.contains("technical lead"));

    for (role, profile) in [
        (AgentRole::Labour, AgentProfile::Coding),
        (AgentRole::Verifier, AgentProfile::Verification),
        (AgentRole::Scout, AgentProfile::Research),
    ] {
        let spec = help_spec(role, profile);
        let sections = build_prompt_sections(
            &engine,
            &project,
            &session,
            &spec,
            None,
            "task objective",
            true,
            InstructionBuildMode::Materialize,
            None,
        )
        .await
        .unwrap();
        assert!(
            sections
                .exact_text(crate::identity_prompt::IDENTITY_SECTION_KEY)
                .is_none(),
            "{} must not have an identity section",
            role.as_str()
        );
        assert!(sections.identity.is_none());
    }
}

#[tokio::test]
async fn assigned_identity_memory_is_tail_only_and_warm_omits_it() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    enable_identity_policy(&engine, &project.id).await;
    let worker = project_identity(&engine, &project.id, "worker", "Auth Tech Lead", "auth").await;
    let session = engine
        .create_session(project.id.clone(), "memory scope goal".into(), None)
        .await
        .unwrap();
    assign_identity(&engine, &session.id, &worker).await;
    engine
        .create_memory(
            &session.id,
            "decision",
            "Auth boundary",
            "The auth component validates tokens before routing and owns key rotation.",
            vec!["auth".into()],
        )
        .await
        .unwrap();

    let spec = help_spec(AgentRole::Worker, AgentProfile::Coding);
    let warm = build_prompt_sections(
        &engine,
        &project,
        &session,
        &spec,
        None,
        "implement auth",
        false,
        InstructionBuildMode::Preview,
        None,
    )
    .await
    .unwrap();
    assert!(
        warm.exact_text(crate::identity_prompt::IDENTITY_SECTION_KEY)
            .is_some()
    );
    assert!(warm.tail.is_empty());
    assert!(
        warm.exact_text(crate::identity_prompt::IDENTITY_MEMORY_SECTION_KEY)
            .is_none()
    );

    let full = build_prompt_sections(
        &engine,
        &project,
        &session,
        &spec,
        None,
        "implement auth",
        true,
        InstructionBuildMode::Preview,
        None,
    )
    .await
    .unwrap();
    let memory = full
        .exact_text(crate::identity_prompt::IDENTITY_MEMORY_SECTION_KEY)
        .expect("identity memory tail");
    assert!(memory.contains("Identity/department memory"));
    assert!(memory.contains("auth component validates tokens"));
    assert!(!full.render().contains("auth component validates tokens"));
    assert!(full.stable_prefix().contains("Auth Tech Lead"));
    assert!(!full.stable_prefix().contains("identity memory"));

    let labour = help_spec(AgentRole::Labour, AgentProfile::Coding);
    let labour_sections = build_prompt_sections(
        &engine,
        &project,
        &session,
        &labour,
        None,
        "implement auth",
        true,
        InstructionBuildMode::Preview,
        None,
    )
    .await
    .unwrap();
    assert!(
        labour_sections
            .exact_text(crate::identity_prompt::IDENTITY_SECTION_KEY)
            .is_none()
    );
    assert!(
        labour_sections
            .exact_text(crate::identity_prompt::IDENTITY_MEMORY_SECTION_KEY)
            .is_none()
    );
    assert!(
        !labour_sections
            .render()
            .contains("auth component validates tokens")
    );
}

#[tokio::test]
async fn layout_v2_on_an_assigned_session_keeps_legacy_combined_bytes() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    enable_identity_policy(&engine, &project.id).await;
    let worker = project_identity(&engine, &project.id, "worker", "Auth Tech Lead", "auth").await;
    let session = engine
        .create_session(project.id.clone(), "v2 goal".into(), None)
        .await
        .unwrap();
    assign_identity(&engine, &session.id, &worker).await;
    engine
        .set_session_prompt_layout(&session.id, 2)
        .await
        .unwrap();
    let session = engine.store().get_session(&session.id).unwrap().unwrap();
    let spec = help_spec(AgentRole::Worker, AgentProfile::Coding);
    let sections = build_prompt_sections(
        &engine,
        &project,
        &session,
        &spec,
        None,
        "legacy task",
        true,
        InstructionBuildMode::Preview,
        None,
    )
    .await
    .unwrap();
    assert!(!sections.is_v3());
    assert!(sections.head.is_empty());
    assert!(sections.tail.is_empty());
    assert!(sections.identity.is_none());
    assert!(!sections.render().contains("Agent identity"));
    assert!(
        engine
            .store()
            .list_session_instructions(&session.id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn paused_identity_is_not_used_for_a_new_prompt_identity() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    enable_identity_policy(&engine, &project.id).await;
    let leader = project_identity(&engine, &project.id, "leader", "Project Lead", "project").await;
    let session = engine
        .create_session(project.id.clone(), "paused goal".into(), None)
        .await
        .unwrap();
    assign_identity(&engine, &session.id, &leader).await;
    let before = engine
        .prompt_identity_for_run(&session.id, "leader", None, None)
        .await
        .unwrap();
    assert!(before.is_some());
    engine
        .pause_agent_identity(vp_protocol::types::PauseAgentIdentityParams {
            identity_id: leader,
        })
        .await
        .unwrap();
    let after = engine
        .prompt_identity_for_run(&session.id, "leader", None, None)
        .await
        .unwrap();
    assert!(after.is_none());
}

#[test]
fn delegate_role_chain_is_worker_to_labour_and_leaf_roles_fail_closed() {
    let labour = serde_json::json!({"role": "labour", "objective": "implement the handler"});
    let worker = parse_delegate(&labour, AgentRole::Worker, AgentProfile::Coding, None)
        .expect("Worker can delegate implementation to Labour");
    assert_eq!(worker.0, AgentRole::Labour);

    assert!(parse_delegate(&labour, AgentRole::Labour, AgentProfile::Coding, None).is_err());
    assert!(
        parse_delegate(
            &labour,
            AgentRole::Verifier,
            AgentProfile::Verification,
            None
        )
        .is_err()
    );
    assert!(parse_delegate(&labour, AgentRole::Scout, AgentProfile::Research, None).is_err());

    let wrong = serde_json::json!({"role": "worker", "objective": "implement the handler"});
    assert!(parse_delegate(&wrong, AgentRole::Worker, AgentProfile::Coding, None).is_err());
    let manager = serde_json::json!({
        "role": "manager",
        "title": "Discovery",
        "objective": "map the system"
    });
    assert!(parse_delegate(&manager, AgentRole::Leader, AgentProfile::Coding, None).is_ok());
    assert!(parse_delegate(&manager, AgentRole::Worker, AgentProfile::Coding, None).is_err());
}

#[test]
fn delegated_runs_receive_role_identity_sections_and_worker_can_delegate_to_labour() {
    run_with_large_stack(|| async {
        let dir = TempDir::new().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn Provider> = Arc::new(RecordingProvider {
            requests: requests.clone(),
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
                            "objective": "lead the readme implementation"
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
        enable_identity_policy(&engine, &project.id).await;
        let leader =
            project_identity(&engine, &project.id, "leader", "Project Lead", "project").await;
        let manager =
            project_identity(&engine, &project.id, "manager", "Discovery", "discovery").await;
        let worker =
            project_identity(&engine, &project.id, "worker", "Auth Tech Lead", "auth").await;
        let session = engine
            .create_session(project.id.clone(), "identity delegation".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test-model")
            .await
            .unwrap();
        assign_identity(&engine, &session.id, &leader).await;
        assign_identity(&engine, &session.id, &manager).await;
        assign_identity(&engine, &session.id, &worker).await;
        service
            .start(&session.id, "delegate with identities")
            .await
            .unwrap();
        let finished = wait_until_idle(&engine, &session).await;
        assert_eq!(finished.status, "idle");

        let captured = requests.lock().unwrap().clone();
        assert!(captured.len() >= 8);
        let identity_request = |role: &str| {
            captured
                .iter()
                .find(|request| {
                    request.system_sections.iter().any(|section| {
                        section.kind == "identity"
                            && section.text.contains(&format!("Role: {role}"))
                    })
                })
                .unwrap_or_else(|| panic!("missing {role} identity request"))
        };
        let leader_request = identity_request("leader");
        let manager_request = captured
            .iter()
            .filter(|request| {
                request.system_sections.iter().any(|section| {
                    section.kind == "identity" && section.text.contains("Role: manager")
                })
            })
            .find(|request| request.tools.iter().any(|tool| tool.name == "delegate"))
            .unwrap_or_else(|| {
                let manager_requests = captured
                    .iter()
                    .filter(|request| {
                        request.system_sections.iter().any(|section| {
                            section.kind == "identity"
                                && section.text.contains("Role: manager")
                        })
                    })
                    .count();
                panic!(
                    "no delegating Manager request among {manager_requests} manager requests; tool sets: {:?}",
                    captured
                        .iter()
                        .map(|request| request
                            .tools
                            .iter()
                            .any(|tool| tool.name == "delegate"))
                        .collect::<Vec<_>>()
                )
            });
        let worker_request = identity_request("worker");
        for (request, marker) in [
            (leader_request, "project and portfolio owner"),
            (manager_request, "department architect"),
            (worker_request, "technical lead"),
        ] {
            let identity = request
                .system_sections
                .iter()
                .find(|section| section.kind == "identity")
                .expect("identity section");
            assert!(identity.text.contains(marker));
            assert!(!identity.text.contains("delegate with identities"));
        }
        assert!(
            worker_request
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("delegate(role=\"labour\")")
        );
        let worker_delegate = worker_request
            .tools
            .iter()
            .find(|tool| tool.name == "delegate")
            .expect("Worker delegate schema");
        assert_eq!(
            worker_delegate.parameters["properties"]["role"]["enum"],
            serde_json::json!(["labour"])
        );
        let manager_delegate = manager_request
            .tools
            .iter()
            .find(|tool| tool.name == "delegate")
            .unwrap_or_else(|| {
                panic!(
                    "Manager delegate schema missing; manager system starts {:?}, tools {:?}",
                    manager_request
                        .system
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect::<String>(),
                    manager_request
                        .tools
                        .iter()
                        .map(|tool| tool.name.as_str())
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            manager_delegate.parameters["properties"]["role"]["enum"],
            serde_json::json!(["worker"])
        );
        assert!(
            manager_request
                .tools
                .iter()
                .any(|tool| tool.name == "delegate_many"),
            "Manager must retain delegate_many"
        );
        assert!(
            manager_request
                .tools
                .iter()
                .all(|tool| tool.name != "write_file" && tool.name != "shell")
        );

        let labour_request = captured
            .iter()
            .find(|request| {
                request
                    .system
                    .as_deref()
                    .unwrap_or_default()
                    .contains("You are Labour")
                    && request
                        .system_sections
                        .iter()
                        .all(|section| section.kind != "identity")
            })
            .expect("Labour request without identity");
        assert!(
            labour_request
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("implementation worker")
        );
        assert!(
            labour_request
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("cannot delegate")
        );
        assert!(
            labour_request
                .tools
                .iter()
                .all(|tool| tool.name != "delegate"),
            "Labour must not advertise delegate"
        );
        assert!(
            labour_request
                .tail_sections
                .iter()
                .all(|section| section.kind != "agent/memory")
        );

        let report = engine
            .session_instructions(&session.id, None)
            .await
            .unwrap();
        let identity_states = report
            .states
            .iter()
            .filter(|state| state.identity_hash.is_some())
            .count();
        assert!(
            identity_states >= 3,
            "expected leader/manager/worker identity states, got {identity_states}"
        );
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("Project Lead"));
        assert!(!encoded.contains("Auth Tech Lead"));
        assert!(!encoded.contains("Discovery"));
    });
}

#[test]
fn role_tool_surface_encodes_the_delegation_and_write_boundaries() {
    let surface = |role, profile| {
        role_tool_surface(role, profile)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let leader = surface(AgentRole::Leader, AgentProfile::Coding);
    let manager = surface(AgentRole::Manager, AgentProfile::Generic);
    let worker = surface(AgentRole::Worker, AgentProfile::Coding);
    let labour = surface(AgentRole::Labour, AgentProfile::Coding);
    let verifier = surface(AgentRole::Verifier, AgentProfile::Verification);
    let scout = surface(AgentRole::Scout, AgentProfile::Research);

    for name in ["delegate", "delegate_acp", "scout"] {
        assert!(leader.contains(name), "Leader should retain {name}");
        assert!(manager.contains(name), "Manager should retain {name}");
        assert!(!verifier.contains(name), "Verifier must not get {name}");
        assert!(!scout.contains(name), "Scout must not get {name}");
        assert!(!labour.contains(name), "Labour must not get {name}");
    }
    assert!(manager.contains("delegate_many"));
    assert!(!worker.contains("delegate_many"));
    assert!(!labour.contains("delegate_many"));
    assert!(!verifier.contains("delegate_many"));
    assert!(!scout.contains("delegate_many"));
    assert!(worker.contains("delegate"));

    for name in ["write_file", "edit_file", "apply_patch", "shell", "process"] {
        assert!(
            !manager.contains(name),
            "Manager must not have workspace tool {name}"
        );
        assert!(
            !scout.contains(name),
            "Scout must not have workspace tool {name}"
        );
    }
    for name in ["write_file", "edit_file", "apply_patch"] {
        assert!(
            !verifier.contains(name),
            "Verifier must not have write tool {name}"
        );
    }
    // The Verifier may run tests through shell/process; it still has no write
    // or merge path, and the Verifier run uses a detached read-only clone.
    assert!(verifier.contains("shell"));
    assert!(verifier.contains("process"));
    for name in ["write_file", "edit_file", "apply_patch", "shell"] {
        assert!(worker.contains(name), "Worker needs {name}");
        assert!(labour.contains(name), "Labour needs {name}");
    }
    assert!(worker.contains("team_status"));
    assert!(labour.contains("team_status"));
    assert!(labour.contains("coordinate"));
    assert!(labour.contains("send_message"));
    assert!(!labour.contains("task_control"));
    assert!(manager.contains("task_control"));
    assert!(leader.contains("submit_team_plan"));
    assert!(!manager.contains("submit_team_plan"));
    assert!(!worker.contains("submit_team_plan"));
    assert!(!labour.contains("submit_verdict"));
    assert!(verifier.contains("submit_verdict"));
}

#[test]
fn worker_and_labour_identity_roles_stay_distinct_from_execution_roles() {
    let worker_context = crate::identity_prompt::role_contract("worker").unwrap();
    assert_eq!(worker_context.role, "worker");
    assert!(worker_context.delegation.contains("Labour"));
    let labour_contract = crate::identity_prompt::role_contract("labour");
    assert!(labour_contract.is_none());
    let verifier_contract = crate::identity_prompt::role_contract("verifier");
    assert!(verifier_contract.is_none());
    let scout_contract = crate::identity_prompt::role_contract("scout");
    assert!(scout_contract.is_none());
    assert_eq!(AgentRole::Worker.child_role(), Some(AgentRole::Labour));
    assert_eq!(AgentRole::Labour.child_role(), None);
}

#[tokio::test]
async fn identity_memory_change_moves_only_the_tail_not_the_prefix_or_epoch() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    enable_identity_policy(&engine, &project.id).await;
    let worker = project_identity(&engine, &project.id, "worker", "Auth Tech Lead", "auth").await;
    let session = engine
        .create_session(project.id.clone(), "tail-only memory".into(), None)
        .await
        .unwrap();
    assign_identity(&engine, &session.id, &worker).await;
    let spec = help_spec(AgentRole::Worker, AgentProfile::Coding);

    let mut before_meta = None;
    let before = build_prompt_sections(
        &engine,
        &project,
        &session,
        &spec,
        None,
        "implement auth",
        true,
        InstructionBuildMode::Preview,
        Some(&mut before_meta),
    )
    .await
    .unwrap();
    assert!(before_meta.is_none());
    assert!(
        before
            .exact_text(crate::identity_prompt::IDENTITY_MEMORY_SECTION_KEY)
            .is_none()
    );
    let before_hashes = before.hashes("test-model", &[]);
    let before_epoch = crate::cache_epoch::CacheEpoch::from_hashes("test-model", &before_hashes);

    let worker_identity = engine
        .prompt_identity_for_run(&session.id, "worker", None, None)
        .await
        .unwrap()
        .expect("assigned worker");
    engine
        .remember_agent_memory(vp_protocol::types::CreateAgentMemoryParams {
            project_id: project.id.clone(),
            identity_id: Some(worker_identity.id),
            scope: "agent".to_string(),
            content: "Auth validates tokens before routing.".to_string(),
            evidence: vec![vp_protocol::types::MemoryEvidence {
                kind: "task".to_string(),
                reference: "T1".to_string(),
                note: None,
            }],
            source: Some("operator".to_string()),
            ..vp_protocol::types::CreateAgentMemoryParams::default()
        })
        .await
        .unwrap();

    let mut after_meta = None;
    let after = build_prompt_sections(
        &engine,
        &project,
        &session,
        &spec,
        None,
        "implement auth",
        true,
        InstructionBuildMode::Preview,
        Some(&mut after_meta),
    )
    .await
    .unwrap();
    let after_hashes = after.hashes("test-model", &[]);
    let after_epoch = crate::cache_epoch::CacheEpoch::from_hashes("test-model", &after_hashes);
    assert_eq!(before_hashes.core_hash, after_hashes.core_hash);
    assert_eq!(before_hashes.project_hash, after_hashes.project_hash);
    assert_eq!(before_hashes.tools_hash, after_hashes.tools_hash);
    assert_eq!(
        before_hashes.stable_prefix_hash,
        after_hashes.stable_prefix_hash
    );
    assert_eq!(before_hashes.prefix_hash, after_hashes.prefix_hash);
    assert_eq!(before_hashes.identity_hash, after_hashes.identity_hash);
    assert_eq!(before_epoch.hash, after_epoch.hash);
    assert_ne!(before_hashes.volatile_hash, after_hashes.volatile_hash);
    assert!(after_hashes.section_chars.contains_key("agent/memory"));
    let memory_section = after
        .exact_text(crate::identity_prompt::IDENTITY_MEMORY_SECTION_KEY)
        .expect("identity memory tail");
    assert!(memory_section.contains("Auth validates tokens"));
    let meta = after_meta.expect("memory metadata");
    assert_eq!(meta.source, "agent_memory");
    assert_eq!(meta.memory_count, 1);
    assert!(meta.brief_chars > 0);
    let telemetry = meta.telemetry().expect("telemetry");
    let serialized = serde_json::to_string(&telemetry).unwrap();
    assert!(!serialized.contains("Auth validates tokens"));
    assert_eq!(telemetry.memory_count, 1);
    assert_eq!(telemetry.memory_hashes.len(), 1);

    // A warm sends only the stable head; the memory tail is omitted entirely.
    let mut warm_meta = Some(meta.clone());
    let warm = build_prompt_sections(
        &engine,
        &project,
        &session,
        &spec,
        None,
        "implement auth",
        false,
        InstructionBuildMode::Preview,
        Some(&mut warm_meta),
    )
    .await
    .unwrap();
    assert!(warm_meta.is_none());
    assert!(
        warm.exact_text(crate::identity_prompt::IDENTITY_MEMORY_SECTION_KEY)
            .is_none()
    );
}

#[tokio::test]
async fn remember_and_recall_scope_defaults_and_typed_scope_errors() {
    let dir = TempDir::new().unwrap();
    let (engine, _service, project) = test_service(&dir);
    enable_identity_policy(&engine, &project.id).await;
    let worker = project_identity(&engine, &project.id, "worker", "Auth Tech Lead", "auth").await;
    let session = engine
        .create_session(project.id.clone(), "scoped tools".into(), None)
        .await
        .unwrap();
    assign_identity(&engine, &session.id, &worker).await;
    let spec = help_spec(AgentRole::Worker, AgentProfile::Coding);

    // Omitted scope remains the legacy project-memory path, exactly as before.
    let legacy = run_memory_tool(
        &engine,
        &session,
        &project,
        &spec,
        "remember",
        serde_json::json!({
            "kind": "decision",
            "title": "Legacy decision",
            "body": "The legacy store remains the default."
        }),
    )
    .await
    .unwrap();
    assert!(!legacy.is_error, "{}", legacy.content);
    assert!(legacy.content.contains("[decision]"));
    let legacy_rows = engine
        .list_memories(&project.id, None, None, 10)
        .await
        .unwrap()
        .memories;
    assert_eq!(legacy_rows.len(), 1);
    let agent_rows = engine
        .list_agent_memories(vp_protocol::types::AgentMemoryListParams {
            project_id: project.id.clone(),
            ..vp_protocol::types::AgentMemoryListParams::default()
        })
        .await
        .unwrap();
    assert!(agent_rows.memories.is_empty());

    let scoped = run_memory_tool(
        &engine,
        &session,
        &project,
        &spec,
        "remember",
        serde_json::json!({
            "kind": "decision",
            "title": "Auth boundary",
            "body": "Auth owns token validation.",
            "scope": "agent",
            "evidence": [{"kind": "commit", "reference": "abcdef123456"}]
        }),
    )
    .await
    .unwrap();
    assert!(!scoped.is_error, "{}", scoped.content);
    assert!(scoped.content.contains("[agent]"));
    let rows = engine
        .list_agent_memories(vp_protocol::types::AgentMemoryListParams {
            project_id: project.id.clone(),
            identity_id: Some(worker.clone()),
            ..vp_protocol::types::AgentMemoryListParams::default()
        })
        .await
        .unwrap()
        .memories;
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .evidence
            .iter()
            .any(|entry| entry.kind == "session" && entry.reference == session.id)
    );
    assert!(
        rows[0]
            .evidence
            .iter()
            .any(|entry| entry.kind == "commit" && entry.reference == "abcdef123456")
    );

    let recall = run_memory_tool(
        &engine,
        &session,
        &project,
        &spec,
        "recall",
        serde_json::json!({"query": "token", "scope": "agent"}),
    )
    .await
    .unwrap();
    assert!(!recall.is_error, "{}", recall.content);
    assert!(recall.content.contains("Auth owns token validation"));
    assert!(recall.content.contains("(commit abcdef1)"));

    // Explicit scope=project is the org-memory store, not a silent redirect of
    // the legacy default.
    let project_recall = run_memory_tool(
        &engine,
        &session,
        &project,
        &spec,
        "recall",
        serde_json::json!({"query": "legacy", "scope": "project"}),
    )
    .await
    .unwrap();
    assert!(
        project_recall
            .content
            .contains("No matching identity memories.")
    );

    let invalid = run_memory_tool(
        &engine,
        &session,
        &project,
        &spec,
        "remember",
        serde_json::json!({
            "kind": "fact",
            "title": "bad",
            "body": "scope is not free-form",
            "scope": "session"
        }),
    )
    .await
    .unwrap();
    assert!(invalid.is_error);
    assert!(invalid.content.contains("MEMORY_SCOPE_INVALID"));

    // Task-scoped roles have no project identity; agent/department scope fails
    // closed instead of falling back to project memory.
    let labour = help_spec(AgentRole::Labour, AgentProfile::Coding);
    let labour_attempt = run_memory_tool(
        &engine,
        &session,
        &project,
        &labour,
        "remember",
        serde_json::json!({
            "kind": "fact",
            "title": "labour memory",
            "body": "must not switch scope silently",
            "scope": "agent"
        }),
    )
    .await
    .unwrap();
    assert!(labour_attempt.is_error);
    assert!(labour_attempt.content.contains("MEMORY_IDENTITY_REQUIRED"));
    let legacy_count = engine
        .list_memories(&project.id, None, None, 10)
        .await
        .unwrap()
        .memories
        .len();
    assert_eq!(
        legacy_count, 1,
        "failed agent scope must not write legacy memory"
    );
}

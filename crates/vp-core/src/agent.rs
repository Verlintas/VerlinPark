use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_recursion::async_recursion;

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;
use vp_protocol::types::{
    AgentInfo, ConfigureAgentParams, Project, Session, StoredMessage, TodoItem,
};
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, StreamEvent,
    ToolCall, ToolSchema,
};
use vp_store::Store;

use crate::Engine;
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};
use crate::worktree;

#[derive(Clone)]
pub struct AgentConfig {
    pub provider: Arc<dyn Provider>,
    pub provider_name: String,
    pub model: String,
    pub base_url: Option<String>,
    pub max_turns: u32,
    pub max_tokens: Option<u32>,
    pub max_managers: usize,
    pub max_workers: usize,
    pub max_labour: usize,
    pub tools: ToolRegistry,
}

#[derive(Clone)]
pub struct AgentService {
    engine: Engine,
    config: Arc<RwLock<Option<AgentConfig>>>,
    runs: Arc<Mutex<HashMap<String, Arc<RunControl>>>>,
}

struct RunControl {
    cancelled: watch::Sender<bool>,
}

impl RunControl {
    fn new() -> Self {
        let (cancelled, _) = watch::channel(false);
        Self { cancelled }
    }

    fn cancel(&self) {
        let _ = self.cancelled.send(true);
    }

    fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.cancelled.subscribe()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AgentRole {
    Leader,
    Manager,
    Worker,
    Labour,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Manager => "manager",
            Self::Worker => "worker",
            Self::Labour => "labour",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "leader" => Some(Self::Leader),
            "manager" => Some(Self::Manager),
            "worker" => Some(Self::Worker),
            "labour" => Some(Self::Labour),
            _ => None,
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Self::Leader => "Leader",
            Self::Manager => "Manager",
            Self::Worker => "Worker",
            Self::Labour => "Labour",
        }
    }

    fn child_role(self) -> Option<Self> {
        match self {
            Self::Leader => Some(Self::Manager),
            Self::Manager => Some(Self::Worker),
            Self::Worker => Some(Self::Labour),
            Self::Labour => None,
        }
    }

    fn allows_tool(self, name: &str) -> bool {
        match self {
            Self::Manager => matches!(name, "read_file" | "list_dir" | "grep"),
            Self::Leader | Self::Worker | Self::Labour => {
                matches!(
                    name,
                    "read_file" | "list_dir" | "grep" | "edit_file" | "write_file" | "shell"
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
struct AgentSpec {
    id: String,
    role: AgentRole,
    name: String,
    depth: u8,
    parent_name: Option<String>,
    worktree: PathBuf,
    branch: String,
    session_worktree: PathBuf,
    session_branch: String,
    write_scope: Vec<String>,
    base_commit: String,
}

struct SessionBudget {
    limit: Option<u64>,
    caps: AgentCaps,
    state: Mutex<BudgetState>,
}

#[derive(Clone, Copy)]
struct AgentCaps {
    managers: usize,
    workers: usize,
    labour: usize,
}

struct BudgetState {
    spent: u64,
    managers: usize,
    workers: usize,
    labour: usize,
}

impl SessionBudget {
    fn new(limit: Option<u64>, spent: u64, caps: AgentCaps) -> Self {
        Self {
            limit,
            caps,
            state: Mutex::new(BudgetState {
                spent,
                managers: 0,
                workers: 0,
                labour: 0,
            }),
        }
    }

    fn spent(&self) -> u64 {
        self.state.lock().map(|state| state.spent).unwrap_or(0)
    }

    fn exhausted(&self) -> bool {
        self.limit.is_some_and(|limit| self.spent() >= limit)
    }

    fn add_usage(&self, input: u64, output: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return true;
        };
        state.spent = state.spent.saturating_add(input).saturating_add(output);
        self.limit.is_some_and(|limit| state.spent >= limit)
    }

    fn reserve(&self, role: AgentRole) -> Option<usize> {
        let mut state = self.state.lock().ok()?;
        let (counter, cap) = match role {
            AgentRole::Manager => (&mut state.managers, self.caps.managers),
            AgentRole::Worker => (&mut state.workers, self.caps.workers),
            AgentRole::Labour => (&mut state.labour, self.caps.labour),
            AgentRole::Leader => return None,
        };
        if *counter >= cap {
            return None;
        }
        *counter += 1;
        Some(*counter)
    }
}

enum LoopOutcome {
    Final(String),
    Cancelled,
    BudgetExceeded,
}

fn agent_fields(payload: &mut serde_json::Value, spec: &AgentSpec) {
    if let Some(object) = payload.as_object_mut() {
        object.insert("agent_id".into(), serde_json::json!(spec.id));
        object.insert("agent_role".into(), serde_json::json!(spec.role.as_str()));
        object.insert("agent_name".into(), serde_json::json!(spec.name));
        object.insert("agent_depth".into(), serde_json::json!(spec.depth));
    }
}

async fn emit(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    kind: &str,
    mut payload: serde_json::Value,
) -> Result<()> {
    agent_fields(&mut payload, spec);
    engine.append_event(session_id, kind, payload).await?;
    Ok(())
}

impl AgentService {
    pub fn from_env(engine: Engine) -> Self {
        let base_url = env::var("VERLINPARK_BASE_URL")
            .ok()
            .or_else(|| env::var("OPENAI_BASE_URL").ok());
        let api_key = env::var("VERLINPARK_API_KEY")
            .ok()
            .or_else(|| env::var("OPENAI_API_KEY").ok());
        let model = env::var("VERLINPARK_MODEL")
            .ok()
            .or_else(|| env::var("OPENAI_MODEL").ok());

        if base_url.is_none() && api_key.is_none() {
            return Self::unconfigured(engine);
        }

        let base_url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let model = model.unwrap_or_else(|| "gpt-4o-mini".to_string());
        let provider = Arc::new(OpenAiCompatibleProvider::new(
            "openai-compatible",
            base_url.clone(),
            api_key,
        ));
        Self::configured(
            engine,
            AgentConfig {
                provider,
                provider_name: "openai-compatible".to_string(),
                model,
                base_url: Some(base_url),
                max_turns: env::var("VERLINPARK_MAX_TURNS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(20),
                max_tokens: env::var("VERLINPARK_MAX_TOKENS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .or(Some(4096)),
                max_managers: env::var("VERLINPARK_MAX_MANAGERS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(5),
                max_workers: env::var("VERLINPARK_MAX_WORKERS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(10),
                max_labour: env::var("VERLINPARK_MAX_LABOUR")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(20),
                tools: ToolRegistry::default_tools(),
            },
        )
    }

    pub fn unconfigured(engine: Engine) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(None)),
            runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn configured(engine: Engine, config: AgentConfig) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(Some(config))),
            runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn configure(&self, params: ConfigureAgentParams) -> Result<AgentInfo> {
        let model = params.model.trim().to_string();
        if model.is_empty() {
            return Err(anyhow!("model is required"));
        }
        let base_url = params
            .base_url
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| match params.provider.as_str() {
                "anthropic" => "https://api.anthropic.com/v1".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            });
        let provider: Arc<dyn Provider> = match params.provider.as_str() {
            "openai-compatible" => Arc::new(OpenAiCompatibleProvider::new(
                "openai-compatible",
                base_url.clone(),
                params.api_key,
            )),
            "anthropic" => Arc::new(AnthropicProvider::with_base_url(
                "anthropic",
                base_url.clone(),
                params.api_key,
            )),
            other => return Err(anyhow!("unsupported provider: {other}")),
        };
        let config = AgentConfig {
            provider,
            provider_name: params.provider,
            model,
            base_url: Some(base_url),
            max_turns: params.max_turns.unwrap_or(20).clamp(1, 200),
            max_tokens: params.max_tokens.or(Some(4096)),
            max_managers: params.max_managers.unwrap_or(5).clamp(1, 5) as usize,
            max_workers: params.max_workers.unwrap_or(10).clamp(1, 10) as usize,
            max_labour: params.max_labour.unwrap_or(20).clamp(1, 40) as usize,
            tools: ToolRegistry::default_tools(),
        };
        *self
            .config
            .write()
            .map_err(|_| anyhow!("agent config lock poisoned"))? = Some(config);
        Ok(self.info())
    }

    pub fn info(&self) -> AgentInfo {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        match config {
            Some(config) => AgentInfo {
                configured: true,
                provider: Some(config.provider_name.clone()),
                model: Some(config.model.clone()),
                base_url: config.base_url.clone(),
            },
            None => AgentInfo {
                configured: false,
                provider: None,
                model: None,
                base_url: None,
            },
        }
    }

    pub async fn start(&self, session_id: &str, prompt: &str) -> Result<()> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            self.engine
                .append_event(
                    session_id,
                    "agent.unavailable",
                    serde_json::json!({
                        "message": "No model provider configured. Set VERLINPARK_BASE_URL and VERLINPARK_MODEL (and an API key if needed)."
                    }),
                )
                .await?;
            return Err(anyhow!(
                "agent provider is not configured; set VERLINPARK_BASE_URL and VERLINPARK_MODEL"
            ));
        };

        let control = Arc::new(RunControl::new());
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(session_id) {
                return Err(anyhow!("session is already running"));
            }
            runs.insert(session_id.to_string(), control.clone());
        }

        let prepared: Result<()> = async {
            self.engine
                .update_session_status(session_id, "running")
                .await?;
            let user_message = self
                .engine
                .append_message(session_id, "user", prompt, serde_json::json!([]), None)
                .await?;
            self.engine
                .append_event(
                    session_id,
                    "message.created",
                    serde_json::json!({
                        "role": "user",
                        "content": user_message.content,
                    }),
                )
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(session_id);
            }
            return Err(error);
        }

        let engine = self.engine.clone();
        let session_id = session_id.to_string();
        let prompt = prompt.to_string();
        let runs = self.runs.clone();
        tokio::spawn(async move {
            let result = run_agent(
                engine.clone(),
                config,
                &session_id,
                &prompt,
                control.clone(),
            )
            .await;
            if let Err(error) = &result {
                let _ = engine
                    .append_event(
                        &session_id,
                        "agent.failed",
                        serde_json::json!({"error": error.to_string()}),
                    )
                    .await;
                let _ = engine.update_session_status(&session_id, "failed").await;
            }
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&session_id);
            }
        });
        Ok(())
    }

    pub fn cancel(&self, session_id: &str) -> bool {
        if let Ok(runs) = self.runs.lock()
            && let Some(control) = runs.get(session_id)
        {
            control.cancel();
            return true;
        }
        false
    }
}

async fn run_agent(
    engine: Engine,
    config: AgentConfig,
    session_id: &str,
    prompt: &str,
    control: Arc<RunControl>,
) -> Result<()> {
    let store = engine.store().clone();
    let (_, project) = load_session_project(store.clone(), session_id).await?;
    let session = engine.ensure_session_worktree(session_id).await?;
    let worktree = PathBuf::from(
        session
            .worktree
            .clone()
            .ok_or_else(|| anyhow!("session has no worktree"))?,
    );
    let session_branch = session.branch.clone().unwrap_or_default();
    let leader = engine
        .list_agents(session_id)
        .await?
        .into_iter()
        .find(|agent| agent.role == "leader");
    let leader_id = leader.as_ref().map(|agent| agent.id.clone());
    let spec = AgentSpec {
        id: leader_id.clone().unwrap_or_else(|| "leader".to_string()),
        role: AgentRole::Leader,
        name: leader
            .map(|agent| agent.name)
            .unwrap_or_else(|| "Leader".to_string()),
        depth: 0,
        parent_name: None,
        worktree: worktree.clone(),
        branch: session_branch.clone(),
        session_worktree: worktree.clone(),
        session_branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),
    };
    let budget = SessionBudget::new(
        session.token_budget,
        session.tokens_in + session.tokens_out,
        AgentCaps {
            managers: config.max_managers,
            workers: config.max_workers,
            labour: config.max_labour,
        },
    );
    if budget.exhausted() {
        emit(
            &engine,
            session_id,
            &spec,
            "agent.budget_exceeded",
            serde_json::json!({
                "limit": session.token_budget,
                "spent": budget.spent(),
            }),
        )
        .await?;
        engine.update_session_status(session_id, "failed").await?;
        return Ok(());
    }
    if let Some(agent_id) = &leader_id {
        engine.update_agent_status(agent_id, "running").await?;
    }

    emit(
        &engine,
        session_id,
        &spec,
        "agent.started",
        serde_json::json!({
            "goal": session.goal,
            "prompt": prompt,
            "model": config.model,
            "provider": config.provider_name,
            "worktree": session.worktree,
            "branch": session.branch,
        }),
    )
    .await?;

    let mut conversation = load_conversation(&engine, session_id).await?;
    let result = run_loop(
        &engine,
        &config,
        &session,
        &project,
        &control,
        &budget,
        &spec,
        prompt,
        &mut conversation,
        true,
    )
    .await;
    if result.is_err()
        && let Some(agent_id) = &leader_id
    {
        let _ = engine.update_agent_status(agent_id, "failed").await;
    }
    match result? {
        LoopOutcome::Final(_) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "idle").await?;
            }
            engine.update_session_status(session_id, "idle").await?;
        }
        LoopOutcome::Cancelled => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "cancelled").await?;
            }
            finish_cancelled(&engine, session_id).await?;
        }
        LoopOutcome::BudgetExceeded => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "failed").await?;
            }
            emit(
                &engine,
                session_id,
                &spec,
                "agent.budget_exceeded",
                serde_json::json!({
                    "limit": session.token_budget,
                    "spent": budget.spent(),
                }),
            )
            .await?;
            engine.update_session_status(session_id, "failed").await?;
        }
    }
    Ok(())
}

fn normalize_scope(scope: &str) -> String {
    scope
        .trim()
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/')
        .to_string()
}

fn path_in_scope(path: &str, scopes: &[String]) -> bool {
    if scopes.is_empty() {
        return true;
    }
    scopes.iter().any(|scope| {
        let scope = normalize_scope(scope);
        scope == "*" || scope == "**" || path == scope || path.starts_with(&format!("{scope}/"))
    })
}

fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return true;
    }
    left.iter().any(|a| {
        let a = normalize_scope(a);
        right.iter().any(|b| {
            let b = normalize_scope(b);
            a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
        })
    })
}

struct IntegrationResult {
    commit: Option<String>,
    merged: bool,
}

async fn integrate_worker(spec: &AgentSpec, name: &str) -> Result<IntegrationResult> {
    let task_worktree = spec.worktree.clone();
    let session_worktree = spec.session_worktree.clone();
    let branch = spec.branch.clone();
    let message = format!("task: {name}");
    let merge_message = format!("merge {name}");
    let write_scope = spec.write_scope.clone();
    let base_commit = spec.base_commit.clone();
    tokio::task::spawn_blocking(move || -> Result<IntegrationResult> {
        if !write_scope.is_empty() {
            let paths = worktree::changed_paths(&task_worktree, &base_commit)?;
            let outside = paths
                .iter()
                .filter(|path| !path_in_scope(path, &write_scope))
                .cloned()
                .collect::<Vec<_>>();
            if !outside.is_empty() {
                return Err(anyhow!("write scope violation: {}", outside.join(", ")));
            }
        }
        let commit = worktree::commit_all(&task_worktree, &message)?;
        let Some(commit) = commit else {
            return Ok(IntegrationResult {
                commit: None,
                merged: false,
            });
        };
        worktree::merge_branch(&session_worktree, &branch, &merge_message)?;
        Ok(IntegrationResult {
            commit: Some(commit),
            merged: true,
        })
    })
    .await?
}

#[allow(clippy::too_many_arguments)]
#[async_recursion]
async fn run_loop(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    objective: &str,
    conversation: &mut Vec<ChatMessage>,
    top_level: bool,
) -> Result<LoopOutcome> {
    let tool_context = ToolContext {
        root: spec.worktree.clone(),
    };
    let system = role_system_prompt(spec.role, project, session, spec, objective);
    let tools = tool_schemas(config, spec.role);

    for turn in 0..config.max_turns {
        if control.is_cancelled() {
            return Ok(LoopOutcome::Cancelled);
        }
        if budget.exhausted() {
            return Ok(LoopOutcome::BudgetExceeded);
        }
        let request = ChatRequest {
            model: config.model.clone(),
            messages: conversation.clone(),
            system: Some(system.clone()),
            max_tokens: config.max_tokens,
            temperature: Some(0.2),
            tools: tools.clone(),
        };
        let mut stream = config
            .provider
            .stream_chat(request)
            .await
            .context("starting provider stream")?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = (0u32, 0u32);
        let mut cancelled = false;
        let mut cancel_rx = control.subscribe();
        loop {
            if control.is_cancelled() {
                cancelled = true;
                break;
            }
            let item = tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_err() || *cancel_rx.borrow() {
                        cancelled = true;
                        break;
                    }
                    continue;
                }
                item = stream.next() => item,
            };
            let Some(item) = item else {
                break;
            };
            match item? {
                StreamEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "agent.message_delta",
                        serde_json::json!({"text": delta}),
                    )
                    .await?;
                }
                StreamEvent::ToolCall(call) => tool_calls.push(call),
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    usage = (usage.0 + input_tokens, usage.1 + output_tokens);
                }
                StreamEvent::Done => {}
            }
        }
        if cancelled {
            return Ok(LoopOutcome::Cancelled);
        }
        if usage != (0, 0) {
            engine
                .record_usage(&session.id, usage.0 as u64, usage.1 as u64)
                .await?;
            if budget.add_usage(usage.0 as u64, usage.1 as u64) {
                return Ok(LoopOutcome::BudgetExceeded);
            }
        }

        if text.is_empty() && tool_calls.is_empty() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1, "empty": true}),
            )
            .await?;
            return Ok(LoopOutcome::Final(String::new()));
        }

        let assistant_tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.clone())
        };
        if top_level {
            engine
                .append_message(
                    &session.id,
                    "assistant",
                    &text,
                    serde_json::to_value(assistant_tool_calls.as_deref().unwrap_or(&[]))?,
                    None,
                )
                .await?;
        }
        emit(
            engine,
            &session.id,
            spec,
            "agent.message",
            serde_json::json!({
                "text": text,
                "tool_calls": tool_calls,
            }),
        )
        .await?;
        conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: text.clone(),
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
        });

        if tool_calls.is_empty() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1}),
            )
            .await?;
            return Ok(LoopOutcome::Final(text));
        }

        for (index, call) in tool_calls.into_iter().enumerate() {
            if control.is_cancelled() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded);
            }
            let call_id = if call.id.is_empty() {
                format!("call_{}_{}", turn, index)
            } else {
                call.id.clone()
            };
            let input: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
            let is_delegate = call.name == "delegate";
            let is_delegate_many = call.name == "delegate_many" && spec.role == AgentRole::Manager;
            let is_todo = call.name == "todo_write" && spec.role == AgentRole::Leader;
            let registry_tool = config
                .tools
                .get(&call.name)
                .filter(|_| spec.role.allows_tool(&call.name));
            let requires_approval = if is_delegate || is_delegate_many || is_todo {
                false
            } else {
                registry_tool
                    .as_ref()
                    .map(|tool| tool.requires_approval())
                    .unwrap_or(false)
            };

            let mut approved = true;
            if requires_approval {
                let mut context = serde_json::json!({});
                agent_fields(&mut context, spec);
                let (approval, receiver) = engine
                    .request_approval(
                        &session.id,
                        &call_id,
                        &call.name,
                        input.clone(),
                        Some(context),
                    )
                    .await?;
                match wait_for_approval(receiver, control).await {
                    ApprovalOutcome::Approved => {}
                    ApprovalOutcome::Denied => approved = false,
                    ApprovalOutcome::TimedOut => {
                        approved = false;
                        let _ = engine.resolve_approval(&approval.id, false).await;
                    }
                    ApprovalOutcome::Cancelled => {
                        let _ = engine.resolve_approval(&approval.id, false).await;
                        return Ok(LoopOutcome::Cancelled);
                    }
                }
            }

            let output = if !approved {
                ToolOutput::error("The user denied this tool call.")
            } else if is_delegate_many {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_parallel_workers(
                    engine, config, session, project, control, budget, spec, &input,
                )
                .await?
                {
                    ParallelResult::Output(output) => output,
                    ParallelResult::Cancelled => return Ok(LoopOutcome::Cancelled),
                    ParallelResult::BudgetExceeded => {
                        return Ok(LoopOutcome::BudgetExceeded);
                    }
                }
            } else if is_delegate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_delegate(&input, spec.role) {
                    Ok((role, name, child_objective)) => match budget.reserve(role) {
                        None => ToolOutput::error(format!(
                            "the {} cap for this session has been reached",
                            role.as_str()
                        )),
                        Some(ordinal) => {
                            let child_name = name
                                .unwrap_or_else(|| format!("{} {}", role.default_name(), ordinal));
                            let child = engine
                                .create_agent(
                                    &session.id,
                                    role.as_str(),
                                    &child_name,
                                    Some(spec.id.clone()),
                                    &config.model,
                                )
                                .await?;
                            let write_scope = parse_write_scope(&input)?;
                            let (child_worktree, child_branch, child_base, child_scope) = match role
                            {
                                AgentRole::Worker => {
                                    let info = engine
                                        .create_task_worktree(
                                            &project.root,
                                            &project.id,
                                            &session.id,
                                            &child.id,
                                            &spec.session_branch,
                                        )
                                        .await?;
                                    (info.path, info.branch, info.base_commit, write_scope)
                                }
                                AgentRole::Manager => (
                                    spec.session_worktree.clone(),
                                    spec.session_branch.clone(),
                                    spec.base_commit.clone(),
                                    Vec::new(),
                                ),
                                AgentRole::Labour => (
                                    spec.worktree.clone(),
                                    spec.branch.clone(),
                                    spec.base_commit.clone(),
                                    spec.write_scope.clone(),
                                ),
                                AgentRole::Leader => (
                                    spec.session_worktree.clone(),
                                    spec.session_branch.clone(),
                                    spec.base_commit.clone(),
                                    Vec::new(),
                                ),
                            };
                            engine
                                .set_agent_worktree(
                                    &child.id,
                                    &child_worktree.to_string_lossy(),
                                    &child_branch,
                                )
                                .await?;
                            let child_spec = AgentSpec {
                                id: child.id.clone(),
                                role,
                                name: child_name.clone(),
                                depth: spec.depth.saturating_add(1),
                                parent_name: Some(spec.name.clone()),
                                worktree: child_worktree,
                                branch: child_branch,
                                session_worktree: spec.session_worktree.clone(),
                                session_branch: spec.session_branch.clone(),
                                write_scope: child_scope,
                                base_commit: child_base,
                            };
                            emit(
                                engine,
                                &session.id,
                                &child_spec,
                                "agent.spawned",
                                serde_json::json!({
                                    "parent_id": spec.id,
                                    "parent_name": spec.name,
                                    "objective": truncate(child_objective.as_str()),
                                }),
                            )
                            .await?;
                            let mut child_conversation =
                                vec![ChatMessage::user(child_objective.clone())];
                            let child_result = run_loop(
                                engine,
                                config,
                                session,
                                project,
                                control,
                                budget,
                                &child_spec,
                                &child_objective,
                                &mut child_conversation,
                                false,
                            )
                            .await;
                            match child_result {
                                Ok(LoopOutcome::Final(summary)) => {
                                    let summary = if summary.trim().is_empty() {
                                        "The agent completed without a summary.".to_string()
                                    } else {
                                        summary
                                    };
                                    let integration = if role == AgentRole::Worker {
                                        integrate_worker(&child_spec, &child_name).await
                                    } else {
                                        Ok(IntegrationResult {
                                            commit: None,
                                            merged: false,
                                        })
                                    };
                                    match integration {
                                        Ok(integration) => {
                                            engine.update_agent_status(&child.id, "idle").await?;
                                            emit(
                                                engine,
                                                &session.id,
                                                &child_spec,
                                                "agent.finished",
                                                serde_json::json!({
                                                    "status": "idle",
                                                    "summary": truncate(&summary),
                                                    "commit": integration.commit,
                                                    "merged": integration.merged,
                                                    "worktree": child_spec.worktree,
                                                    "branch": child_spec.branch,
                                                }),
                                            )
                                            .await?;
                                            ToolOutput::ok(format!(
                                                "{} {} finished:\n{}\nmerged: {}",
                                                role.default_name(),
                                                child_name,
                                                truncate(&summary),
                                                integration.merged
                                            ))
                                        }
                                        Err(error) => {
                                            engine.update_agent_status(&child.id, "failed").await?;
                                            emit(
                                                engine,
                                                &session.id,
                                                &child_spec,
                                                "agent.finished",
                                                serde_json::json!({
                                                    "status": "failed",
                                                    "summary": truncate(error.to_string().as_str()),
                                                    "worktree": child_spec.worktree,
                                                    "branch": child_spec.branch,
                                                }),
                                            )
                                            .await?;
                                            ToolOutput::error(format!(
                                                "{} {} finished but integration failed: {error}",
                                                role.default_name(),
                                                child_name
                                            ))
                                        }
                                    }
                                }
                                Ok(LoopOutcome::Cancelled) => {
                                    engine.update_agent_status(&child.id, "cancelled").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({"status": "cancelled"}),
                                    )
                                    .await?;
                                    return Ok(LoopOutcome::Cancelled);
                                }
                                Ok(LoopOutcome::BudgetExceeded) => {
                                    engine.update_agent_status(&child.id, "failed").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({
                                            "status": "failed",
                                            "summary": "token budget exhausted",
                                        }),
                                    )
                                    .await?;
                                    return Ok(LoopOutcome::BudgetExceeded);
                                }
                                Err(error) => {
                                    engine.update_agent_status(&child.id, "failed").await?;
                                    emit(
                                        engine,
                                        &session.id,
                                        &child_spec,
                                        "agent.finished",
                                        serde_json::json!({
                                            "status": "failed",
                                            "summary": truncate(error.to_string().as_str()),
                                        }),
                                    )
                                    .await?;
                                    ToolOutput::error(format!(
                                        "{} {} failed: {error}",
                                        role.default_name(),
                                        child_name
                                    ))
                                }
                            }
                        }
                    },
                    Err(error) => ToolOutput::error(error.to_string()),
                }
            } else if is_todo {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_todos(&input) {
                    Ok(items) => match engine.set_todos(&session.id, items).await {
                        Ok(()) => ToolOutput::ok("Updated the session plan.".to_string()),
                        Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                    },
                    Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                }
            } else if let Some(tool) = registry_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let mut cancel_rx = control.subscribe();
                if control.is_cancelled() {
                    return Ok(LoopOutcome::Cancelled);
                }
                tokio::select! {
                    _ = cancel_rx.changed() => {
                        return Ok(LoopOutcome::Cancelled);
                    }
                    result = tool.execute(&tool_context, input.clone()) => match result {
                        Ok(output) => output,
                        Err(error) => ToolOutput::error(format!("tool error: {error}")),
                    },
                }
            } else {
                ToolOutput::error(format!(
                    "tool '{}' is not available to {}",
                    call.name,
                    spec.role.as_str()
                ))
            };

            emit(
                engine,
                &session.id,
                spec,
                "tool.finished",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "output": truncate(&output.content),
                    "is_error": output.is_error,
                }),
            )
            .await?;
            if top_level {
                engine
                    .append_message(
                        &session.id,
                        "tool",
                        &output.content,
                        serde_json::json!([]),
                        Some(call_id.clone()),
                    )
                    .await?;
            }
            conversation.push(ChatMessage::tool(call_id, output.content));
        }
        emit(
            engine,
            &session.id,
            spec,
            "agent.turn_finished",
            serde_json::json!({"turn": turn + 1}),
        )
        .await?;
    }

    Err(anyhow!("maximum turns reached"))
}

fn tool_schemas(config: &AgentConfig, role: AgentRole) -> Vec<ToolSchema> {
    let mut tools = config
        .tools
        .schemas()
        .into_iter()
        .filter(|tool| role.allows_tool(&tool.name))
        .collect::<Vec<_>>();
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "todo_write".to_string(),
            description: "Replace the session plan/todo list. Use as work progresses.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": 50,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        });
    }
    if role.child_role().is_some() {
        tools.push(ToolSchema {
            name: "delegate".to_string(),
            description: format!(
                "Delegate a focused objective to a {}. The child agent runs synchronously and returns a summary.",
                role.child_role()
                    .map(AgentRole::as_str)
                    .unwrap_or("worker")
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "enum": ["manager", "worker", "labour"]
                    },
                    "name": {"type": "string"},
                    "objective": {"type": "string"},
                    "instructions": {"type": "string"},
                    "write_scope": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"}
                    }
                },
                "required": ["role", "objective"]
            }),
        });
    }
    if role == AgentRole::Manager {
        tools.push(ToolSchema {
            name: "delegate_many".to_string(),
            description: "Run 1-5 Workers in parallel. Each task must declare a non-overlapping write_scope when more than one task is provided.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 5,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "objective": {"type": "string"},
                                "instructions": {"type": "string"},
                                "write_scope": {
                                    "type": "array",
                                    "maxItems": 20,
                                    "items": {"type": "string"}
                                }
                            },
                            "required": ["objective", "write_scope"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        });
    }
    tools
}

fn parse_delegate(
    input: &serde_json::Value,
    parent_role: AgentRole,
) -> Result<(AgentRole, Option<String>, String)> {
    let role_name = input
        .get("role")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("role is required"))?;
    let role = AgentRole::parse(role_name).ok_or_else(|| anyhow!("unknown role: {role_name}"))?;
    let expected = parent_role
        .child_role()
        .ok_or_else(|| anyhow!("{} cannot delegate", parent_role.as_str()))?;
    if role != expected {
        return Err(anyhow!(
            "{} can only delegate to {}",
            parent_role.as_str(),
            expected.as_str()
        ));
    }
    let objective = input
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty() && objective.len() <= 4000)
        .ok_or_else(|| anyhow!("objective is required (max 4000 chars)"))?
        .to_string();
    let name = input
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 80)
        .map(ToOwned::to_owned);
    let instructions = input
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty() && instructions.len() <= 4000);
    let objective = match instructions {
        Some(instructions) => format!("{objective}\n\nInstructions:\n{instructions}"),
        None => objective,
    };
    Ok((role, name, objective))
}

fn parse_write_scope(input: &serde_json::Value) -> Result<Vec<String>> {
    let Some(items) = input.get("write_scope") else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| anyhow!("write_scope must be an array"))?;
    if items.len() > 20 {
        return Err(anyhow!("at most 20 write scopes are allowed"));
    }
    let mut scopes = Vec::with_capacity(items.len());
    for item in items {
        let scope = item
            .as_str()
            .ok_or_else(|| anyhow!("write_scope entries must be strings"))?;
        let scope = normalize_scope(scope);
        if scope.is_empty()
            || scope.contains("..")
            || scope.starts_with('/')
            || scope.starts_with('~')
        {
            return Err(anyhow!("invalid write scope: {scope}"));
        }
        scopes.push(scope);
    }
    Ok(scopes)
}

struct ParallelTask {
    name: Option<String>,
    objective: String,
    scope: Vec<String>,
}

enum ParallelResult {
    Output(ToolOutput),
    Cancelled,
    BudgetExceeded,
}

fn parse_parallel_tasks(input: &serde_json::Value) -> Result<Vec<ParallelTask>> {
    let tasks = input
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("tasks is required"))?;
    if tasks.is_empty() || tasks.len() > 5 {
        return Err(anyhow!("delegate_many accepts 1 to 5 tasks"));
    }
    let mut parsed = Vec::with_capacity(tasks.len());
    for task in tasks {
        let objective = task
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|objective| !objective.is_empty() && objective.len() <= 4000)
            .ok_or_else(|| anyhow!("each task needs an objective (max 4000 chars)"))?;
        let instructions = task
            .get("instructions")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty() && instructions.len() <= 4000);
        let objective = match instructions {
            Some(instructions) => format!("{objective}\n\nInstructions:\n{instructions}"),
            None => objective.to_string(),
        };
        let name = task
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 80)
            .map(ToOwned::to_owned);
        let scope = parse_write_scope(task)?;
        if tasks.len() > 1 && scope.is_empty() {
            return Err(anyhow!(
                "parallel Workers require a non-empty write_scope for every task"
            ));
        }
        parsed.push(ParallelTask {
            name,
            objective,
            scope,
        });
    }
    for (index, left) in parsed.iter().enumerate() {
        for right in parsed.iter().skip(index + 1) {
            if scopes_overlap(&left.scope, &right.scope) {
                return Err(anyhow!(
                    "write scopes overlap: {} and {}",
                    left.scope.join(","),
                    right.scope.join(",")
                ));
            }
        }
    }
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
async fn run_parallel_workers(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    input: &serde_json::Value,
) -> Result<ParallelResult> {
    let tasks = parse_parallel_tasks(input)?;
    let mut prepared = Vec::new();
    for task in tasks {
        if control.is_cancelled() {
            return Ok(ParallelResult::Cancelled);
        }
        let Some(ordinal) = budget.reserve(AgentRole::Worker) else {
            return Ok(ParallelResult::Output(ToolOutput::error(
                "the Worker cap for this session has been reached",
            )));
        };
        let child_name = task
            .name
            .clone()
            .unwrap_or_else(|| format!("Worker {}", ordinal));
        let child = engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                &child_name,
                Some(spec.id.clone()),
                &config.model,
            )
            .await?;
        let info = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                &child.id,
                &spec.session_branch,
            )
            .await?;
        engine
            .set_agent_worktree(&child.id, &info.path.to_string_lossy(), &info.branch)
            .await?;
        let child_spec = AgentSpec {
            id: child.id.clone(),
            role: AgentRole::Worker,
            name: child_name.clone(),
            depth: spec.depth.saturating_add(1),
            parent_name: Some(spec.name.clone()),
            worktree: info.path,
            branch: info.branch,
            session_worktree: spec.session_worktree.clone(),
            session_branch: spec.session_branch.clone(),
            write_scope: task.scope.clone(),
            base_commit: info.base_commit,
        };
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.spawned",
            serde_json::json!({
                "parent_id": spec.id,
                "parent_name": spec.name,
                "objective": truncate(&task.objective),
                "write_scope": child_spec.write_scope,
            }),
        )
        .await?;
        let conversation = vec![ChatMessage::user(task.objective.clone())];
        prepared.push((child_spec, task.objective, conversation));
    }

    let futures = prepared
        .iter_mut()
        .map(|(child_spec, objective, conversation)| {
            run_loop(
                engine,
                config,
                session,
                project,
                control,
                budget,
                child_spec,
                objective,
                conversation,
                false,
            )
        });
    let results = futures_util::future::join_all(futures).await;

    let mut cancelled = false;
    let mut budget_exceeded = false;
    let mut handled = vec![false; prepared.len()];
    for (index, result) in results.iter().enumerate() {
        handled[index] = true;
        match result {
            Ok(LoopOutcome::Cancelled) => {
                cancelled = true;
                let (child_spec, _, _) = &prepared[index];
                engine
                    .update_agent_status(&child_spec.id, "cancelled")
                    .await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({"status": "cancelled"}),
                )
                .await?;
            }
            Ok(LoopOutcome::BudgetExceeded) => {
                budget_exceeded = true;
                let (child_spec, _, _) = &prepared[index];
                engine.update_agent_status(&child_spec.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": "token budget exhausted",
                    }),
                )
                .await?;
            }
            _ => {}
        }
    }
    if cancelled || budget_exceeded {
        let status = if cancelled { "cancelled" } else { "failed" };
        for (index, (child_spec, _, _)) in prepared.iter().enumerate() {
            if handled[index] {
                continue;
            }
            engine.update_agent_status(&child_spec.id, status).await?;
            emit(
                engine,
                &session.id,
                child_spec,
                "agent.finished",
                serde_json::json!({
                    "status": status,
                    "summary": "batch aborted before integration",
                }),
            )
            .await?;
        }
        return if cancelled {
            Ok(ParallelResult::Cancelled)
        } else {
            Ok(ParallelResult::BudgetExceeded)
        };
    }

    let mut reports = Vec::new();
    for (index, result) in results.into_iter().enumerate() {
        let (child_spec, _, _) = &prepared[index];
        match result {
            Ok(LoopOutcome::Final(summary)) => {
                let summary = if summary.trim().is_empty() {
                    "The agent completed without a summary.".to_string()
                } else {
                    summary
                };
                match integrate_worker(child_spec, &child_spec.name).await {
                    Ok(integration) => {
                        engine.update_agent_status(&child_spec.id, "idle").await?;
                        emit(
                            engine,
                            &session.id,
                            child_spec,
                            "agent.finished",
                            serde_json::json!({
                                "status": "idle",
                                "summary": truncate(&summary),
                                "commit": integration.commit,
                                "merged": integration.merged,
                                "worktree": child_spec.worktree,
                                "branch": child_spec.branch,
                            }),
                        )
                        .await?;
                        reports.push(format!(
                            "{}: merged={}\n{}",
                            child_spec.name,
                            integration.merged,
                            truncate(&summary)
                        ));
                    }
                    Err(error) => {
                        engine.update_agent_status(&child_spec.id, "failed").await?;
                        emit(
                            engine,
                            &session.id,
                            child_spec,
                            "agent.finished",
                            serde_json::json!({
                                "status": "failed",
                                "summary": truncate(error.to_string().as_str()),
                                "worktree": child_spec.worktree,
                                "branch": child_spec.branch,
                            }),
                        )
                        .await?;
                        reports.push(format!("{}: integration failed: {error}", child_spec.name));
                    }
                }
            }
            Ok(LoopOutcome::Cancelled) | Ok(LoopOutcome::BudgetExceeded) => {}
            Err(error) => {
                engine.update_agent_status(&child_spec.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": truncate(error.to_string().as_str()),
                    }),
                )
                .await?;
                reports.push(format!("{}: failed: {error}", child_spec.name));
            }
        }
    }
    Ok(ParallelResult::Output(ToolOutput::ok(reports.join("\n\n"))))
}

fn role_system_prompt(
    role: AgentRole,
    project: &Project,
    session: &Session,
    spec: &AgentSpec,
    objective: &str,
) -> String {
    let worktree = spec.worktree.to_string_lossy();
    let scope_note = if spec.write_scope.is_empty() {
        "Write scope: unrestricted within the worktree.".to_string()
    } else {
        format!(
            "Write scope: {}. Changes outside this scope are rejected before integration.",
            spec.write_scope.join(", ")
        )
    };
    let base = format!(
        "Project: '{}' at {}. Session goal: '{}'. Worktree: {}. Branch: {}. {} \
         The project root is read-only; all writes stay inside the worktree. \
         Do not claim success without evidence.",
        project.name, project.root, session.goal, worktree, spec.branch, scope_note
    );
    match role {
        AgentRole::Leader => format!(
            "{base}\n\
             You are VerlinPark's Leader agent, {}. Keep a short plan with todo_write. \
             For a goal that spans multiple areas, delegate department-level objectives to \
             Managers with delegate(role=\"manager\"). Managers will split work and delegate \
             to Workers; Workers may use Labour. Do not delegate directly to Workers or Labour. \
             You can still inspect and edit files yourself for small tasks. \
             Review summaries, verify the final diff, and report the result.",
            spec.name
        ),
        AgentRole::Manager => format!(
            "{base}\n\
             You are Manager {}, reporting to {}. Objective: {objective}. \
             You may read and search, but you must not edit files or run shell commands. \
             Split the objective into focused Worker tasks. For independent tasks, use \
             delegate_many(tasks=[...]) with non-overlapping write_scope values so Workers \
             run in parallel; otherwise use delegate(role=\"worker\"). Review the returned \
             summaries, resolve gaps, then return a concise consolidated result. \
             Do not delegate to Managers or Labour.",
            spec.name,
            spec.parent_name.as_deref().unwrap_or("the Leader")
        ),
        AgentRole::Worker => format!(
            "{base}\n\
             You are Worker {}, reporting to a Manager. Task: {objective}. \
             You own implementation in the worktree. Inspect first, make focused edits, \
             and run tests. Delegate small research or verification subtasks to Labour with \
             delegate(role=\"labour\") when useful. Return a concise summary with evidence. \
             Do not delegate to Managers or Workers.",
            spec.name
        ),
        AgentRole::Labour => format!(
            "{base}\n\
             You are Labour {}, reporting to a Worker. Subtask: {objective}. \
             Execute it directly in the worktree and return a concise report. \
             You cannot delegate to other agents.",
            spec.name
        ),
    }
}

async fn load_session_project(store: Store, session_id: &str) -> Result<(Session, Project)> {
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || -> Result<(Session, Project)> {
        let session = store
            .get_session(&session_id)?
            .ok_or_else(|| anyhow!("session not found"))?;
        let project = store
            .get_project(&session.project_id)?
            .ok_or_else(|| anyhow!("project not found"))?;
        Ok((session, project))
    })
    .await?
}

async fn load_conversation(engine: &Engine, session_id: &str) -> Result<Vec<ChatMessage>> {
    let messages: Vec<StoredMessage> = engine.list_messages(session_id).await?;
    Ok(messages
        .into_iter()
        .map(|message| {
            let tool_calls: Vec<ToolCall> =
                serde_json::from_value(message.tool_calls).unwrap_or_default();
            ChatMessage {
                role: message.role,
                content: message.content,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: message.tool_call_id,
            }
        })
        .collect())
}

fn parse_todos(input: &serde_json::Value) -> Result<Vec<TodoItem>> {
    let items = input
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("items is required"))?;
    if items.len() > 50 {
        return Err(anyhow!("at most 50 todo items are allowed"));
    }
    let mut todos = Vec::with_capacity(items.len());
    for item in items {
        let title = item
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty() && title.len() <= 200)
            .ok_or_else(|| anyhow!("each item needs a non-empty title (max 200 chars)"))?;
        let status = item
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending");
        if !matches!(status, "pending" | "in_progress" | "completed") {
            return Err(anyhow!("status must be pending, in_progress, or completed"));
        }
        todos.push(TodoItem {
            title: title.to_string(),
            status: status.to_string(),
        });
    }
    Ok(todos)
}

async fn finish_cancelled(engine: &Engine, session_id: &str) -> Result<()> {
    engine
        .append_event(session_id, "agent.cancelled", serde_json::json!({}))
        .await?;
    engine
        .update_session_status(session_id, "cancelled")
        .await?;
    Ok(())
}

enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
    Cancelled,
}

async fn wait_for_approval(
    receiver: oneshot::Receiver<bool>,
    control: &RunControl,
) -> ApprovalOutcome {
    if control.is_cancelled() {
        return ApprovalOutcome::Cancelled;
    }
    let mut cancel_rx = control.subscribe();
    tokio::select! {
        _ = cancel_rx.changed() => ApprovalOutcome::Cancelled,
        result = timeout(Duration::from_secs(600), receiver) => match result {
            Ok(Ok(true)) => ApprovalOutcome::Approved,
            Ok(Ok(false)) | Ok(Err(_)) => ApprovalOutcome::Denied,
            Err(_) => ApprovalOutcome::TimedOut,
        },
    }
}

fn truncate(content: &str) -> String {
    const MAX: usize = 8_000;
    if content.len() > MAX {
        format!("{}… (truncated)", &content[..MAX])
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
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
            base_url: None,
            max_turns: 4,
            max_tokens: Some(1024),
            max_managers: 5,
            max_workers: 10,
            max_labour: 20,
            tools: ToolRegistry::default_tools(),
        };
        let service = AgentService::configured(engine.clone(), config);
        (engine, service, project)
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
                api_key: Some("secret".to_string()),
                max_turns: None,
                max_tokens: None,
                max_managers: None,
                max_workers: None,
                max_labour: None,
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
                api_key: None,
                max_turns: None,
                max_tokens: None,
                max_managers: None,
                max_workers: None,
                max_labour: None,
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
        tokio::time::sleep(Duration::from_millis(400)).await;

        let agents = engine.list_agents(&session.id).await.unwrap();
        let roles = agents
            .iter()
            .map(|agent| agent.role.as_str())
            .collect::<Vec<_>>();
        assert!(roles.contains(&"leader"));
        assert!(roles.contains(&"manager"));
        assert!(roles.contains(&"worker"));
        assert!(roles.contains(&"labour"));
        assert!(
            agents
                .iter()
                .filter(|agent| agent.role != "leader")
                .all(|agent| agent.status == "idle")
        );

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
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
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

        service.start(&session.id, "integrate").await.unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        approver.abort();

        let worktree = session.worktree.clone().unwrap();
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
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
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
        tokio::time::sleep(Duration::from_millis(600)).await;

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
        tokio::time::sleep(Duration::from_millis(700)).await;
        approver.abort();

        let worktree = session.worktree.clone().unwrap();
        assert!(
            !std::path::Path::new(&worktree)
                .join("docs/out.txt")
                .exists()
        );
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker.status, "failed");
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
}

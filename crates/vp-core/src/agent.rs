use std::collections::HashSet;
use std::env;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use tokio::sync::oneshot;
use tokio::time::timeout;
use vp_protocol::types::{AgentInfo, ConfigureAgentParams, Project, Session, StoredMessage};
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, StreamEvent,
    ToolCall,
};
use vp_store::Store;

use crate::Engine;
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};

#[derive(Clone)]
pub struct AgentConfig {
    pub provider: Arc<dyn Provider>,
    pub provider_name: String,
    pub model: String,
    pub base_url: Option<String>,
    pub max_turns: u32,
    pub max_tokens: Option<u32>,
    pub tools: ToolRegistry,
}

#[derive(Clone)]
pub struct AgentService {
    engine: Engine,
    config: Arc<RwLock<Option<AgentConfig>>>,
    running: Arc<Mutex<HashSet<String>>>,
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
                tools: ToolRegistry::default_tools(),
            },
        )
    }

    pub fn unconfigured(engine: Engine) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(None)),
            running: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn configured(engine: Engine, config: AgentConfig) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(Some(config))),
            running: Arc::new(Mutex::new(HashSet::new())),
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

        {
            let mut running = self
                .running
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if !running.insert(session_id.to_string()) {
                return Err(anyhow!("session is already running"));
            }
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
            if let Ok(mut running) = self.running.lock() {
                running.remove(session_id);
            }
            return Err(error);
        }

        let engine = self.engine.clone();
        let session_id = session_id.to_string();
        let prompt = prompt.to_string();
        let running = self.running.clone();
        tokio::spawn(async move {
            let result = run_agent(engine.clone(), config, &session_id, &prompt).await;
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
            if let Ok(mut running) = running.lock() {
                running.remove(&session_id);
            }
        });
        Ok(())
    }
}

async fn run_agent(
    engine: Engine,
    config: AgentConfig,
    session_id: &str,
    prompt: &str,
) -> Result<()> {
    let store = engine.store().clone();
    let (session, project) = load_session_project(store.clone(), session_id).await?;
    let context = ToolContext {
        root: std::path::PathBuf::from(&project.root),
    };

    engine
        .append_event(
            session_id,
            "agent.started",
            serde_json::json!({
                "goal": session.goal,
                "prompt": prompt,
                "model": config.model,
                "provider": config.provider_name,
            }),
        )
        .await?;

    let mut conversation = load_conversation(&engine, session_id).await?;
    let system = system_prompt(&project, &session);
    let tools = config.tools.schemas();

    for turn in 0..config.max_turns {
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
        while let Some(item) = stream.next().await {
            match item? {
                StreamEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    engine
                        .append_event(
                            session_id,
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
        if usage != (0, 0) {
            engine
                .append_event(
                    session_id,
                    "agent.usage",
                    serde_json::json!({
                        "input_tokens": usage.0,
                        "output_tokens": usage.1,
                    }),
                )
                .await?;
        }

        if text.is_empty() && tool_calls.is_empty() {
            engine.update_session_status(session_id, "idle").await?;
            engine
                .append_event(
                    session_id,
                    "agent.turn_finished",
                    serde_json::json!({"turn": turn + 1, "empty": true}),
                )
                .await?;
            return Ok(());
        }

        let assistant_tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.clone())
        };
        engine
            .append_message(
                session_id,
                "assistant",
                &text,
                serde_json::to_value(assistant_tool_calls.as_deref().unwrap_or(&[]))?,
                None,
            )
            .await?;
        engine
            .append_event(
                session_id,
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
            engine.update_session_status(session_id, "idle").await?;
            engine
                .append_event(
                    session_id,
                    "agent.turn_finished",
                    serde_json::json!({"turn": turn + 1}),
                )
                .await?;
            return Ok(());
        }

        for (index, call) in tool_calls.into_iter().enumerate() {
            let call_id = if call.id.is_empty() {
                format!("call_{}_{}", turn, index)
            } else {
                call.id.clone()
            };
            let tool = config.tools.get(&call.name);
            let input: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
            let requires_approval = tool
                .as_ref()
                .map(|tool| tool.requires_approval())
                .unwrap_or(true);

            let mut approved = true;
            if requires_approval {
                let (approval, receiver) = engine
                    .request_approval(session_id, &call_id, &call.name, input.clone())
                    .await?;
                match wait_for_approval(receiver).await {
                    ApprovalOutcome::Approved => {}
                    ApprovalOutcome::Denied => approved = false,
                    ApprovalOutcome::TimedOut => {
                        approved = false;
                        let _ = engine.resolve_approval(&approval.id, false).await;
                    }
                }
            }

            let output = if !approved {
                ToolOutput {
                    content: "The user denied this tool call.".to_string(),
                    is_error: true,
                }
            } else if let Some(tool) = tool {
                engine
                    .append_event(
                        session_id,
                        "tool.started",
                        serde_json::json!({
                            "tool_call_id": call_id,
                            "name": call.name,
                            "input": input,
                        }),
                    )
                    .await?;
                match tool.execute(&context, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput {
                        content: format!("tool error: {error}"),
                        is_error: true,
                    },
                }
            } else {
                ToolOutput {
                    content: format!("Unknown tool: {}", call.name),
                    is_error: true,
                }
            };

            engine
                .append_event(
                    session_id,
                    "tool.finished",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "output": truncate(&output.content),
                        "is_error": output.is_error,
                    }),
                )
                .await?;
            engine
                .append_message(
                    session_id,
                    "tool",
                    &output.content,
                    serde_json::json!([]),
                    Some(call_id.clone()),
                )
                .await?;
            conversation.push(ChatMessage::tool(call_id, output.content));
        }
    }

    Err(anyhow!("maximum turns reached"))
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

enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
}

async fn wait_for_approval(receiver: oneshot::Receiver<bool>) -> ApprovalOutcome {
    match timeout(Duration::from_secs(600), receiver).await {
        Ok(Ok(true)) => ApprovalOutcome::Approved,
        Ok(Ok(false)) | Ok(Err(_)) => ApprovalOutcome::Denied,
        Err(_) => ApprovalOutcome::TimedOut,
    }
}

fn system_prompt(project: &Project, session: &Session) -> String {
    format!(
        "You are VerlinPark's Leader agent for the project '{}' at {}. \
         The session goal is: {}. \
         Work directly in the project root. Use read_file/list_dir/grep to inspect code. \
         Use write_file only when needed and shell for tests/builds. \
         Write and shell calls require user approval, so explain what you are doing briefly. \
         Prefer small, verifiable steps. When done, summarize the changes and test results. \
         Do not claim success without evidence.",
        project.name, project.root, session.goal
    )
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
        let store = Store::open(dir.path()).unwrap();
        let engine = Engine::new(store);
        let project_dir = dir.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("README.md"), "hello from test\n").unwrap();
        let project = engine
            .store()
            .add_project(project_dir.to_str().unwrap(), Some("repo"))
            .unwrap();
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
        let config = AgentConfig {
            provider,
            provider_name: "queue".into(),
            model: "test".into(),
            base_url: None,
            max_turns: 4,
            max_tokens: Some(1024),
            tools: ToolRegistry::default_tools(),
        };
        let service = AgentService::configured(engine.clone(), config);
        (engine, service, project)
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
            })
            .unwrap_err();
        assert!(error.to_string().contains("model is required"));
    }

    #[tokio::test]
    async fn runs_tool_call_and_final_message() {
        let dir = TempDir::new().unwrap();
        let (engine, service, project) = test_service(&dir);
        let session = engine
            .create_session(project.id, "inspect the repo".into())
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
}

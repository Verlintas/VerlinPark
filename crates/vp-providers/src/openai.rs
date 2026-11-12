use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::canonical::{
    ProviderCacheCapabilities, ProviderWire, TailSystemCapability,
    looks_like_tail_system_rejection, openai_wire_messages,
};
use crate::{
    ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall, openai_usage,
    provider_client, provider_error_from_stream,
};

/// Adapter for OpenAI's chat completions API and compatible endpoints:
/// OpenAI, DeepSeek, OpenRouter, LM Studio, Ollama, llama.cpp server, vLLM.
pub struct OpenAiCompatibleProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    /// Prompt-layout capability, never a routing decision. The default is the
    /// OpenAI-compatible trailing-system form; a server that rejects it is
    /// downgraded to the stable `[memory]` user fallback for later calls.
    capabilities: ProviderCacheCapabilities,
    tail_system_fallback_used: AtomicBool,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            client: provider_client(),
            capabilities: ProviderCacheCapabilities::openai_compatible(),
            tail_system_fallback_used: AtomicBool::new(false),
        }
    }

    pub fn local(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::new(id, base_url, None)
    }

    /// Force the tail-memory shape for a server with unusual role rules. This
    /// is a prompt-layout capability, not model routing.
    pub fn with_tail_system_capability(mut self, capability: TailSystemCapability) -> Self {
        self.capabilities.tail_system = capability;
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn effective_capabilities(&self) -> ProviderCacheCapabilities {
        if self.tail_system_fallback_used.load(Ordering::Relaxed) {
            ProviderCacheCapabilities::new(
                ProviderWire::OpenAiCompatible,
                TailSystemCapability::Unsupported,
            )
        } else {
            self.capabilities
        }
    }

    async fn send_body(
        &self,
        body: &Value,
        preempt: Option<&crate::PreemptToken>,
    ) -> Result<reqwest::Response, ProviderError> {
        crate::send_with_retry_preemptible(
            || {
                let mut builder = self.client.post(self.endpoint()).json(body);
                if let Some(api_key) = &self.api_key {
                    builder = builder.bearer_auth(api_key);
                }
                builder
            },
            preempt,
        )
        .await
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn cache_capabilities(&self) -> ProviderCacheCapabilities {
        self.effective_capabilities()
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        self.stream_chat_inner(request, None).await
    }

    async fn stream_chat_preemptible(
        &self,
        request: ChatRequest,
        token: Option<crate::PreemptToken>,
    ) -> Result<EventStream, ProviderError> {
        self.stream_chat_inner(request, token).await
    }
}

impl OpenAiCompatibleProvider {
    async fn stream_chat_inner(
        &self,
        request: ChatRequest,
        preempt: Option<crate::PreemptToken>,
    ) -> Result<EventStream, ProviderError> {
        let mut capabilities = self.effective_capabilities();
        let mut strip_stream_options = false;
        let mut tail_fallback_attempted = false;
        let response = loop {
            let body = openai_body(&request, capabilities, strip_stream_options);
            match self.send_body(&body, preempt.as_ref()).await {
                Ok(response) => break response,
                Err(ProviderError::Api {
                    status,
                    body: error_body,
                }) if status == 400
                    && !strip_stream_options
                    && body.get("stream_options").is_some()
                    && !looks_like_tail_system_rejection(status, &error_body) =>
                {
                    // Some OpenAI-compatible servers reject optional stream
                    // metadata; fall back once so older local servers still
                    // work. The message bytes are unchanged.
                    strip_stream_options = true;
                }
                Err(ProviderError::Api {
                    status,
                    body: error_body,
                }) if !tail_fallback_attempted
                    && capabilities.tail_system.accepts_system()
                    && looks_like_tail_system_rejection(status, &error_body) =>
                {
                    // A server rejected a mid-history system message. Rebuild
                    // with the stable `[memory]` user fallback. The request
                    // never rewrites earlier content; only the memory anchor's
                    // role changes, and the capability is downgraded for later
                    // calls so the runtime telemetry is honest.
                    self.tail_system_fallback_used
                        .store(true, Ordering::Relaxed);
                    capabilities = self.effective_capabilities();
                    tail_fallback_attempted = true;
                }
                Err(error) => return Err(error),
            }
        };

        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("json"))
        {
            return Err(ProviderError::Unsupported(
                "provider returned a JSON response; expected a stream".into(),
            ));
        }
        let stream = Box::pin(response.bytes_stream().eventsource());
        let preempt = preempt.clone();
        let mapped = async_stream::stream! {
            let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
            let mut saw_done = false;
            let mut stream_failed = false;
            let mut stream = stream;
            loop {
                let next = match &preempt {
                    Some(token) => tokio::select! {
                        _ = token.cancelled() => {
                            yield Err(ProviderError::preempted(token));
                            break;
                        }
                        item = stream.next() => item,
                    },
                    None => stream.next().await,
                };
                let Some(item) = next else {
                    break;
                };
                match item {
                    Err(error) => {
                        yield Err(provider_error_from_stream(error));
                        break;
                    }
                    Ok(event) => {
                        if event.data.trim() == "[DONE]" {
                            saw_done = true;
                            break;
                        }
                        let value: Value = match serde_json::from_str(&event.data) {
                            Ok(value) => value,
                            Err(error) => {
                                yield Err(ProviderError::Parse(error.to_string()));
                                continue;
                            }
                        };
                        if let Some(error) = value.get("error") {
                            stream_failed = true;
                            yield Err(ProviderError::Api {
                                status: 200,
                                body: crate::redact_provider_body(&error.to_string()),
                            });
                            break;
                        }
                        if let Some(text) = value
                            .pointer("/choices/0/delta/content")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                        {
                            yield Ok(StreamEvent::TextDelta(text.to_string()));
                        }
                        if let Some(calls) = value
                            .pointer("/choices/0/delta/tool_calls")
                            .and_then(Value::as_array)
                        {
                            for call in calls {
                                let Some(index) = call.get("index").and_then(Value::as_u64) else {
                                    continue;
                                };
                                let entry = tool_calls
                                    .entry(index as usize)
                                    .or_insert_with(|| ToolCall {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                if let Some(id) = call.get("id").and_then(Value::as_str) {
                                    entry.id = id.to_string();
                                }
                                if let Some(name) = call
                                    .pointer("/function/name")
                                    .and_then(Value::as_str)
                                {
                                    entry.name = name.to_string();
                                }
                                if let Some(arguments) = call
                                    .pointer("/function/arguments")
                                    .and_then(Value::as_str)
                                {
                                    entry.arguments.push_str(arguments);
                                }
                            }
                        }
                        if let Some(usage) = openai_usage(&value) {
                            yield Ok(usage);
                        }
                    }
                }
            }
            if !saw_done && !stream_failed {
                yield Err(ProviderError::Stream(
                    "provider stream ended before [DONE]".into(),
                ));
                return;
            }
            for (index, mut call) in tool_calls {
                if !call.name.is_empty() {
                    if call.id.is_empty() {
                        call.id = crate::fallback_tool_call_id(index);
                    }
                    yield Ok(StreamEvent::ToolCall(call));
                }
            }
            yield Ok(StreamEvent::Done);
        };

        Ok(Box::pin(mapped))
    }
}

fn openai_body(
    request: &ChatRequest,
    capabilities: ProviderCacheCapabilities,
    strip_stream_options: bool,
) -> Value {
    let messages = openai_wire_messages(request, capabilities);
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
    });
    if !strip_stream_options {
        body["stream_options"] = json!({"include_usage": true});
    }
    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if !request.tools.is_empty() {
        // Tool definitions are part of the prefix. Sort by name so a registry
        // iteration-order change cannot silently move the cache boundary or
        // make equivalent runs non-comparable.
        let mut tools = request.tools.iter().collect::<Vec<_>>();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        body["tools"] = json!(
            tools
                .iter()
                .map(|tool| json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                }))
                .collect::<Vec<_>>()
        );
        body["tool_choice"] = json!("auto");
    }
    body
}

/// A deterministic in-memory provider used by tests and dry runs.
pub struct ScriptedProvider {
    id: String,
    events: Vec<StreamEvent>,
}

impl ScriptedProvider {
    pub fn new(id: impl Into<String>, events: Vec<StreamEvent>) -> Self {
        Self {
            id: id.into(),
            events,
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let events = self.events.clone();
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn parses_openai_sse_stream() {
        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        assert_eq!(
            events,
            vec![
                StreamEvent::TextDelta("Hello".to_string()),
                StreamEvent::TextDelta(" world".to_string()),
                StreamEvent::Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
                StreamEvent::Done,
            ]
        );
    }

    #[tokio::test]
    async fn parses_openai_cached_prompt_tokens() {
        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"cached\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":8,\"cache_write_tokens\":2}}}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut usage = None;
        while let Some(item) = stream.next().await {
            if let StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } = item.unwrap()
            {
                usage = Some((
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                ));
            }
        }
        assert_eq!(usage, Some((20, 5, 8, 2)));
    }

    #[tokio::test]
    async fn requests_stream_usage_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                        "data: [DONE]\n\n"
                    )),
            )
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let _ = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[tokio::test]
    async fn parses_streamed_tool_calls() {
        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"README.md\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut request =
            ChatRequest::new("test-model", vec![ChatMessage::user("read the readme")]);
        request.tools = vec![crate::ToolSchema {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object"}),
        }];
        let mut stream = provider.stream_chat(request).await.unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        assert!(events.contains(&StreamEvent::ToolCall(ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: "{\"path\":\"README.md\"}".to_string(),
        })));
        assert_eq!(events.last(), Some(&StreamEvent::Done));
    }

    #[tokio::test]
    async fn errors_when_stream_ends_without_done() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
                    ),
            )
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut error = None;
        while let Some(item) = stream.next().await {
            if let Err(provider_error) = item {
                error = Some(provider_error);
            }
        }
        assert!(matches!(error, Some(ProviderError::Stream(_))));
    }

    #[tokio::test]
    async fn surfaces_provider_error_objects() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("data: {\"error\":{\"message\":\"backend unavailable\"}}\n\n"),
            )
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut error = None;
        while let Some(item) = stream.next().await {
            if let Err(provider_error) = item {
                error = Some(provider_error);
            }
        }
        assert!(matches!(
            error,
            Some(ProviderError::Api { status: 200, .. })
        ));
    }

    #[tokio::test]
    async fn rejects_non_sse_success_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "{\"choices\":[{\"message\":{\"content\":\"ok\"}}]}",
                "application/json",
            ))
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let error = match provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
        {
            Ok(_) => panic!("expected non-SSE rejection"),
            Err(error) => error,
        };
        assert!(matches!(error, ProviderError::Unsupported(_)));
    }

    #[tokio::test]
    async fn assigns_an_id_when_the_provider_omits_one() {
        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut tool_call = None;
        while let Some(item) = stream.next().await {
            if let Ok(StreamEvent::ToolCall(call)) = item {
                tool_call = Some(call);
            }
        }
        let tool_call = tool_call.expect("tool call");
        assert!(tool_call.id.starts_with("call_0_"));
        assert_eq!(tool_call.name, "read_file");
    }

    #[tokio::test]
    async fn retries_transient_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("retry-after", "0")
                    .set_body_string("busy"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\n",
                        "data: [DONE]\n\n"
                    )),
            )
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        assert_eq!(
            events,
            vec![
                StreamEvent::TextDelta("recovered".to_string()),
                StreamEvent::Done,
            ]
        );
    }

    #[tokio::test]
    async fn does_not_retry_auth_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
            .expect(1)
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let error = match provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
        {
            Ok(_) => panic!("expected HTTP error"),
            Err(error) => error,
        };
        assert!(matches!(error, ProviderError::Api { status: 401, .. }));
    }

    async fn capture_openai_body(request: ChatRequest) -> (MockServer, Value) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
                        "data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":7}}}\n\n",
                        "data: [DONE]\n\n"
                    )),
            )
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let _ = provider.stream_chat(request).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body = serde_json::from_slice(&requests[0].body).unwrap();
        (server, body)
    }

    #[tokio::test]
    async fn stable_sections_precede_volatile_and_tools_are_sorted() {
        let mut request =
            ChatRequest::new("test-model", vec![ChatMessage::user("inspect the repo")]);
        request.system_sections = vec![
            crate::SystemSection::new("core", "CORE CHARTER"),
            crate::SystemSection::new("project", "PROJECT SOURCES"),
            crate::SystemSection::new("volatile", "VOLATILE RUN CONTEXT"),
        ];
        request.system =
            Some("CORE CHARTER\n\nPROJECT SOURCES\n\nVOLATILE RUN CONTEXT".to_string());
        request.cache = Some(crate::PromptCacheConfig::new(
            true,
            crate::CacheTtl::FiveMinutes,
            true,
        ));
        request.tools = vec![
            crate::ToolSchema {
                name: "write_file".to_string(),
                description: "Write".to_string(),
                parameters: json!({"type": "object"}),
            },
            crate::ToolSchema {
                name: "apply_patch".to_string(),
                description: "Patch".to_string(),
                parameters: json!({"type": "object"}),
            },
            crate::ToolSchema {
                name: "read_file".to_string(),
                description: "Read".to_string(),
                parameters: json!({"type": "object"}),
            },
        ];
        let (_server, body) = capture_openai_body(request).await;
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[0]["content"], json!("CORE CHARTER"));
        assert_eq!(messages[1]["role"], json!("system"));
        assert_eq!(messages[1]["content"], json!("PROJECT SOURCES"));
        assert_eq!(messages[2]["role"], json!("system"));
        assert_eq!(messages[2]["content"], json!("VOLATILE RUN CONTEXT"));
        assert_eq!(messages[3]["role"], json!("user"));
        assert_eq!(messages[3]["content"], json!("inspect the repo"));
        let names = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["apply_patch", "read_file", "write_file"]);
        // OpenAI-compatible endpoints use provider-managed prefix caching.
        // The adapter must not invent Anthropic-style markers.
        assert!(body.get("cache_control").is_none());
        assert!(!body.to_string().contains("cache_control"));
        assert!(!body.to_string().contains("\"ttl\""));
    }

    #[tokio::test]
    async fn tail_memory_is_a_trailing_system_message_after_the_conversation() {
        let mut request = ChatRequest::new(
            "test-model",
            vec![
                ChatMessage::user("first prompt"),
                ChatMessage::assistant_tool_calls("", Vec::new()),
                ChatMessage::user("second prompt"),
            ],
        );
        request.system_sections = vec![crate::SystemSection::new("core", "CORE")];
        request.system = Some("CORE".to_string());
        request.tail_sections = vec![crate::SystemSection::new("tail/memory", "MEMORY BRIEF")];
        let (_server, body) = capture_openai_body(request).await;
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[0]["content"], json!("CORE"));
        assert_eq!(messages[1]["content"], json!("first prompt"));
        assert_eq!(messages[2]["role"], json!("assistant"));
        assert_eq!(messages[3]["content"], json!("second prompt"));
        assert_eq!(messages[4]["role"], json!("system"));
        assert_eq!(messages[4]["content"], json!("MEMORY BRIEF"));
        // Prior conversation messages are unchanged by the tail section.
        assert!(!messages[1]["content"].as_str().unwrap().contains("MEMORY"));
        assert!(!messages[3]["content"].as_str().unwrap().contains("MEMORY"));
    }

    #[tokio::test]
    async fn volatile_follows_core_when_the_project_section_is_empty() {
        let mut request = ChatRequest::new("test-model", vec![ChatMessage::user("hello")]);
        request.system_sections = vec![
            crate::SystemSection::new("core", "CORE"),
            crate::SystemSection::new("project", ""),
            crate::SystemSection::new("volatile", "VOLATILE"),
        ];
        request.system = Some("CORE\n\nVOLATILE".to_string());
        let (_server, body) = capture_openai_body(request).await;
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["content"], json!("CORE"));
        assert_eq!(messages[1]["content"], json!("VOLATILE"));
        assert_eq!(messages[2]["role"], json!("user"));
    }

    #[tokio::test]
    async fn empty_sections_fall_back_to_the_legacy_system_message() {
        let mut request = ChatRequest::new("test-model", vec![ChatMessage::user("hello")]);
        request.system = Some("LEGACY SYSTEM".to_string());
        request.system_sections = vec![
            crate::SystemSection::new("core", ""),
            crate::SystemSection::new("project", ""),
        ];
        let (_server, body) = capture_openai_body(request).await;
        let messages = body["messages"].as_array().unwrap();
        // Empty sections are not sent as blank system messages; the legacy
        // combined system message is preserved instead.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[0]["content"], json!("LEGACY SYSTEM"));
        assert_eq!(messages[1]["role"], json!("user"));
    }

    #[tokio::test]
    async fn system_section_usage_still_streams_usage_and_completion() {
        let mut request = ChatRequest::new("test-model", vec![ChatMessage::user("hello")]);
        request.system_sections = vec![crate::SystemSection::new("core", "CORE")];
        request.system = Some("CORE".to_string());
        let (_server, body) = capture_openai_body(request).await;
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
        assert_eq!(body["messages"][0]["content"], json!("CORE"));
    }

    #[tokio::test]
    async fn surfaces_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let error = match provider
            .stream_chat(ChatRequest::new(
                "test-model",
                vec![ChatMessage::user("hi")],
            ))
            .await
        {
            Ok(_) => panic!("expected HTTP error"),
            Err(error) => error,
        };
        assert!(matches!(error, ProviderError::Api { status: 401, .. }));
    }

    /// Minimal chunked SSE server: one fast text chunk, then a long pause, so a
    /// preemption can be observed between chunks without a second provider
    /// call or a whole-turn replay.
    async fn spawn_slow_sse() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0u8; 8192];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await;
            let first = "data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n";
            let _ = socket
                .write_all(format!("{:x}\r\n{first}\r\n", first.len()).as_bytes())
                .await;
            let _ = socket.flush().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let second = "data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n";
            let _ = socket
                .write_all(format!("{:x}\r\n{second}\r\n", second.len()).as_bytes())
                .await;
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        address
    }

    #[tokio::test]
    async fn preempts_a_slow_sse_stream_between_chunks() {
        let address = spawn_slow_sse().await;
        let provider = OpenAiCompatibleProvider::local("test", format!("http://{address}/v1"));
        let token = crate::PreemptToken::disabled();
        let mut stream = provider
            .stream_chat_preemptible(
                ChatRequest::new("test-model", vec![ChatMessage::user("hi")]),
                Some(token.clone()),
            )
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let mut text = String::new();
        let mut error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamEvent::TextDelta(delta)) => {
                    text.push_str(&delta);
                    if text == "first" {
                        token.trigger(
                            crate::PreemptReason::Explicit,
                            crate::PreemptPhase::Provider,
                            false,
                        );
                    }
                }
                Ok(_) => {}
                Err(item) => {
                    error = Some(item);
                    break;
                }
            }
        }
        assert_eq!(text, "first");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(matches!(
            error,
            Some(ProviderError::Preempted {
                reason,
                phase,
                ..
            }) if reason == "explicit" && phase == "provider"
        ));
    }

    #[tokio::test]
    async fn preemption_aborts_a_retry_backoff() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("retry-after", "2")
                    .set_body_string("busy"),
            )
            .mount(&server)
            .await;
        let provider = OpenAiCompatibleProvider::local("test", format!("{}/v1", server.uri()));
        let token = crate::PreemptToken::disabled();
        let trigger = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            trigger.trigger(
                crate::PreemptReason::Explicit,
                crate::PreemptPhase::Provider,
                false,
            );
        });
        let started = std::time::Instant::now();
        let error = match provider
            .stream_chat_preemptible(
                ChatRequest::new("test-model", vec![ChatMessage::user("hi")]),
                Some(token),
            )
            .await
        {
            Ok(_) => panic!("expected preemption"),
            Err(error) => error,
        };
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(matches!(error, ProviderError::Preempted { .. }));
    }
}

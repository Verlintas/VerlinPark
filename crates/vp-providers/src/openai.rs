use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};

use std::collections::BTreeMap;

use crate::{
    ChatMessage, ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall,
    openai_usage, provider_client, provider_error_from_stream, send_with_retry,
};

/// Adapter for OpenAI's chat completions API and compatible endpoints:
/// OpenAI, DeepSeek, OpenRouter, LM Studio, Ollama, llama.cpp server, vLLM.
pub struct OpenAiCompatibleProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
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
        }
    }

    pub fn local(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::new(id, base_url, None)
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

#[async_trait]
impl Provider for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let mut messages = Vec::with_capacity(request.messages.len() + 2);
        if let Some(system) = request.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.extend(request.messages.iter().map(openai_message));

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
        });
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
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

        let response = send_with_retry(|| {
            let mut builder = self.client.post(self.endpoint()).json(&body);
            if let Some(api_key) = &self.api_key {
                builder = builder.bearer_auth(api_key);
            }
            builder
        })
        .await?;

        let stream = Box::pin(response.bytes_stream().eventsource());
        let mapped = async_stream::stream! {
            let mut tool_calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
            let mut stream = stream;
            while let Some(item) = stream.next().await {
                match item {
                    Err(error) => {
                        yield Err(provider_error_from_stream(error));
                        break;
                    }
                    Ok(event) => {
                        if event.data.trim() == "[DONE]" {
                            break;
                        }
                        let value: Value = match serde_json::from_str(&event.data) {
                            Ok(value) => value,
                            Err(error) => {
                                yield Err(ProviderError::Parse(error.to_string()));
                                continue;
                            }
                        };
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
            for (_, call) in tool_calls {
                if !call.name.is_empty() {
                    yield Ok(StreamEvent::ToolCall(call));
                }
            }
            yield Ok(StreamEvent::Done);
        };

        Ok(Box::pin(mapped))
    }
}

fn openai_message(message: &ChatMessage) -> Value {
    let mut value = json!({
        "role": message.role,
        "content": message.content,
    });
    if let Some(tool_calls) = &message.tool_calls {
        value["tool_calls"] = json!(
            tool_calls
                .iter()
                .map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": call.arguments,
                    }
                }))
                .collect::<Vec<_>>()
        );
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = json!(tool_call_id);
    }
    value
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
                    output_tokens: 3
                },
                StreamEvent::Done,
            ]
        );
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
}

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};

use std::collections::BTreeMap;

use crate::{
    ChatMessage, ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall,
    provider_client, provider_error_from_stream, send_with_retry,
};

/// Anthropic Messages API adapter.
pub struct AnthropicProvider {
    id: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_base_url(id, "https://api.anthropic.com/v1", Some(api_key.into()))
    }

    pub fn with_base_url(
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

    fn endpoint(&self) -> String {
        format!("{}/messages", self.base_url)
    }
}

fn anthropic_message(message: &ChatMessage) -> Option<Value> {
    if message.role == "tool" {
        let tool_use_id = message.tool_call_id.clone().unwrap_or_default();
        return Some(json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": message.content,
            }]
        }));
    }
    if let Some(tool_calls) = &message.tool_calls {
        let mut blocks = Vec::with_capacity(tool_calls.len() + 1);
        if !message.content.is_empty() {
            blocks.push(json!({"type": "text", "text": message.content}));
        }
        for call in tool_calls {
            let input: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": input,
            }));
        }
        return Some(json!({"role": "assistant", "content": blocks}));
    }
    Some(json!({"role": message.role, "content": message.content}))
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let api_key = self.api_key.as_ref().ok_or(ProviderError::MissingApiKey)?;

        let messages: Vec<Value> = request
            .messages
            .iter()
            .filter(|message| message.role != "system")
            .filter_map(anthropic_message)
            .collect();

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "stream": true,
        });
        if let Some(system) = request.system {
            body["system"] = json!(system);
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
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    }))
                    .collect::<Vec<_>>()
            );
        }

        let response = send_with_retry(|| {
            self.client
                .post(self.endpoint())
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
        })
        .await?;

        let stream = Box::pin(response.bytes_stream().eventsource());
        let mapped = async_stream::stream! {
            let mut input_tokens = 0u32;
            let mut output_tokens = 0u32;
            let mut tool_calls: BTreeMap<u64, ToolCall> = BTreeMap::new();
            let mut stream = stream;
            while let Some(item) = stream.next().await {
                match item {
                    Err(error) => {
                        yield Err(provider_error_from_stream(error));
                        break;
                    }
                    Ok(event) => {
                        let value: Value = match serde_json::from_str(&event.data) {
                            Ok(value) => value,
                            Err(error) => {
                                yield Err(ProviderError::Parse(error.to_string()));
                                continue;
                            }
                        };
                        match event.event.as_str() {
                            "content_block_start" => {
                                if value.pointer("/content_block/type").and_then(Value::as_str)
                                    == Some("tool_use")
                                    && let Some(index) =
                                        value.get("index").and_then(Value::as_u64)
                                {
                                    tool_calls.insert(
                                        index,
                                        ToolCall {
                                            id: value
                                                .pointer("/content_block/id")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                            name: value
                                                .pointer("/content_block/name")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                            arguments: String::new(),
                                        },
                                    );
                                }
                            }
                            "content_block_delta" => {
                                if value.pointer("/delta/type").and_then(Value::as_str)
                                    == Some("input_json_delta")
                                    && let Some(index) =
                                        value.get("index").and_then(Value::as_u64)
                                    && let Some(partial) =
                                        value.pointer("/delta/partial_json").and_then(Value::as_str)
                                    && let Some(call) = tool_calls.get_mut(&index)
                                {
                                    call.arguments.push_str(partial);
                                }
                                if let Some(text) = value
                                    .pointer("/delta/text")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.is_empty())
                                {
                                    yield Ok(StreamEvent::TextDelta(text.to_string()));
                                }
                            }
                            "content_block_stop" => {
                                if let Some(index) = value.get("index").and_then(Value::as_u64)
                                    && let Some(call) = tool_calls.remove(&index)
                                    && !call.name.is_empty()
                                {
                                    yield Ok(StreamEvent::ToolCall(call));
                                }
                            }
                            "message_start" => {
                                if let Some(input) = value
                                    .pointer("/message/usage/input_tokens")
                                    .and_then(Value::as_u64)
                                {
                                    input_tokens = input as u32;
                                }
                            }
                            "message_delta" => {
                                if let Some(output) = value
                                    .pointer("/usage/output_tokens")
                                    .and_then(Value::as_u64)
                                {
                                    output_tokens = output as u32;
                                }
                                yield Ok(StreamEvent::Usage {
                                    input_tokens,
                                    output_tokens,
                                });
                            }
                            "message_stop" => {
                                yield Ok(StreamEvent::Done);
                            }
                            "error" => {
                                yield Err(ProviderError::Api {
                                    status: 200,
                                    body: value.to_string(),
                                });
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        };

        Ok(Box::pin(mapped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;
    use futures_util::StreamExt;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn parses_anthropic_sse_stream() {
        let server = MockServer::start().await;
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" there\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = AnthropicProvider::with_base_url(
            "test",
            format!("{}/v1", server.uri()),
            Some("test-key".to_string()),
        );
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "claude-test",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        assert!(events.contains(&StreamEvent::TextDelta("Hi".to_string())));
        assert!(events.contains(&StreamEvent::TextDelta(" there".to_string())));
        assert!(events.contains(&StreamEvent::Usage {
            input_tokens: 12,
            output_tokens: 3
        }));
        assert_eq!(events.last(), Some(&StreamEvent::Done));
    }
    #[tokio::test]
    async fn retries_rate_limits() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "0")
                    .set_body_string("slow down"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(concat!(
                        "event: content_block_delta\n",
                        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
                        "event: message_stop\n",
                        "data: {\"type\":\"message_stop\"}\n\n",
                    )),
            )
            .mount(&server)
            .await;

        let provider = AnthropicProvider::with_base_url(
            "test",
            format!("{}/v1", server.uri()),
            Some("test-key".to_string()),
        );
        let mut stream = provider
            .stream_chat(ChatRequest::new(
                "claude-test",
                vec![ChatMessage::user("hi")],
            ))
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        assert!(events.contains(&StreamEvent::TextDelta("ok".to_string())));
        assert_eq!(events.last(), Some(&StreamEvent::Done));
    }

    #[tokio::test]
    async fn parses_streamed_tool_use() {
        let server = MockServer::start().await;
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"README.md\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let provider = AnthropicProvider::with_base_url(
            "test",
            format!("{}/v1", server.uri()),
            Some("test-key".to_string()),
        );
        let mut request =
            ChatRequest::new("claude-test", vec![ChatMessage::user("read the readme")]);
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
            id: "toolu_1".to_string(),
            name: "read_file".to_string(),
            arguments: "{\"path\":\"README.md\"}".to_string(),
        })));
        assert_eq!(events.last(), Some(&StreamEvent::Done));
    }
}

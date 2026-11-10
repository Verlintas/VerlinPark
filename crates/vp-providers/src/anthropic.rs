use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};

use std::collections::BTreeMap;

use crate::{
    ChatMessage, ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall,
    ToolSchema, provider_client, provider_error_from_stream, send_with_retry,
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

fn anthropic_system(system: String, stable_prefix: Option<&str>) -> Value {
    let Some(prefix) = stable_prefix.filter(|prefix| {
        !prefix.is_empty() && system.starts_with(prefix) && prefix.len() <= system.len()
    }) else {
        // Keep the historical single-string system shape when no cache
        // breakpoint is requested.
        return json!(system);
    };
    let mut blocks = Vec::with_capacity(2);
    blocks.push(json!({
        "type": "text",
        "text": prefix,
        "cache_control": {"type": "ephemeral"},
    }));
    let remainder = &system[prefix.len()..];
    if !remainder.is_empty() {
        blocks.push(json!({"type": "text", "text": remainder}));
    }
    json!(blocks)
}

fn anthropic_tools(tools: &[ToolSchema]) -> Value {
    let mut values = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect::<Vec<_>>();
    if let Some(last) = values.last_mut() {
        // The tool block is the first part of Anthropic's cache prefix. A
        // breakpoint on the last tool keeps the whole stable bundle reusable
        // without changing the schema when skills activate.
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    json!(values)
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
        let stable_prefix = request.system_stable_prefix.clone();
        if let Some(system) = request.system {
            body["system"] = anthropic_system(system, stable_prefix.as_deref());
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if !request.tools.is_empty() {
            body["tools"] = anthropic_tools(&request.tools);
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
        let mapped = async_stream::stream! {
            let mut input_tokens = 0u32;
            let mut output_tokens = 0u32;
            let mut cache_read_tokens = 0u32;
            let mut cache_write_tokens = 0u32;
            let mut tool_calls: BTreeMap<u64, ToolCall> = BTreeMap::new();
            let mut saw_done = false;
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
                                    && let Some(mut call) = tool_calls.remove(&index)
                                    && !call.name.is_empty()
                                {
                                    if call.id.is_empty() {
                                        call.id = crate::fallback_tool_call_id(index as usize);
                                    }
                                    yield Ok(StreamEvent::ToolCall(call));
                                }
                            }
                            "message_start" => {
                                let usage = value.pointer("/message/usage");
                                if let Some(input) = usage
                                    .and_then(|usage| usage.get("input_tokens"))
                                    .and_then(Value::as_u64)
                                {
                                    input_tokens = input as u32;
                                }
                                if let Some(read) = usage
                                    .and_then(|usage| usage.get("cache_read_input_tokens"))
                                    .and_then(Value::as_u64)
                                {
                                    cache_read_tokens = read as u32;
                                }
                                if let Some(write) = usage
                                    .and_then(|usage| usage.get("cache_creation_input_tokens"))
                                    .and_then(Value::as_u64)
                                {
                                    cache_write_tokens = write as u32;
                                }
                            }
                            "message_delta" => {
                                let usage = value.get("usage");
                                if let Some(output) =
                                    usage.and_then(|usage| usage.get("output_tokens")).and_then(Value::as_u64)
                                {
                                    output_tokens = output as u32;
                                }
                                if let Some(read) = usage
                                    .and_then(|usage| usage.get("cache_read_input_tokens"))
                                    .and_then(Value::as_u64)
                                {
                                    cache_read_tokens = read as u32;
                                }
                                if let Some(write) = usage
                                    .and_then(|usage| usage.get("cache_creation_input_tokens"))
                                    .and_then(Value::as_u64)
                                {
                                    cache_write_tokens = write as u32;
                                }
                                // Anthropic's `input_tokens` excludes cache
                                // reads and writes. Our public contract keeps
                                // `input_tokens` as the total provider input.
                                let total_input = input_tokens
                                    .saturating_add(cache_read_tokens)
                                    .saturating_add(cache_write_tokens);
                                yield Ok(StreamEvent::Usage {
                                    input_tokens: total_input,
                                    output_tokens,
                                    cache_read_tokens,
                                    cache_write_tokens,
                                });
                            }
                            "message_stop" => {
                                saw_done = true;
                                yield Ok(StreamEvent::Done);
                            }
                            "error" => {
                                yield Err(ProviderError::Api {
                                    status: 200,
                                    body: crate::redact_provider_body(&value.to_string()),
                                });
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
            if !saw_done {
                yield Err(ProviderError::Stream(
                    "provider stream ended before message_stop".into(),
                ));
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
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }));
        assert_eq!(events.last(), Some(&StreamEvent::Done));
    }

    #[test]
    fn anthropic_system_keeps_the_string_shape_without_a_marker() {
        assert_eq!(anthropic_system("hello".to_string(), None), json!("hello"));
        assert_eq!(
            anthropic_system("hello".to_string(), Some("not-a-prefix")),
            json!("hello")
        );
    }

    #[tokio::test]
    async fn parses_anthropic_cache_usage_fields() {
        let server = MockServer::start().await;
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":20}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"cached\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":4}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
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
        assert_eq!(usage, Some((132, 4, 100, 20)));
    }

    #[tokio::test]
    async fn marks_tool_and_stable_system_cache_breakpoints() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(concat!(
                        "event: message_start\n",
                        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                        "event: message_delta\n",
                        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
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
        let stable = "Stable role charter.";
        let volatile = "Run context: worktree=/tmp/task.";
        let mut request =
            ChatRequest::new("claude-test", vec![ChatMessage::user("read the readme")]);
        request.system = Some(format!("{stable}\n\n{volatile}"));
        request.system_stable_prefix = Some(stable.to_string());
        request.tools = vec![
            crate::ToolSchema {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: json!({"type": "object"}),
            },
            crate::ToolSchema {
                name: "write_file".to_string(),
                description: "Write a file".to_string(),
                parameters: json!({"type": "object"}),
            },
        ];
        let _ = provider.stream_chat(request).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["system"][0]["text"], stable);
        assert_eq!(
            body["system"][0]["cache_control"]["type"],
            json!("ephemeral")
        );
        assert_eq!(body["system"][1]["text"], format!("\n\n{volatile}"));
        assert!(body["system"][1].get("cache_control").is_none());
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["name"], "write_file");
        assert_eq!(
            body["tools"][1]["cache_control"]["type"],
            json!("ephemeral")
        );
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

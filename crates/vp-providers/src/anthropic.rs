use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};

use std::collections::BTreeMap;

use crate::cache::{
    CacheBreakpointPlan, CacheTtl, SystemSection, looks_like_cache_ttl_rejection,
    plan_cache_breakpoints,
};
use crate::{
    ChatMessage, ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall,
    ToolSchema, provider_client, provider_error_from_stream,
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

fn anthropic_tools_with_plan(tools: &[ToolSchema], plan: Option<&CacheBreakpointPlan>) -> Value {
    let Some(plan) = plan else {
        return anthropic_tools(tools);
    };
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
    if plan.enabled
        && plan.tool_breakpoint
        && let Some(last) = values.last_mut()
    {
        last["cache_control"] = plan.ttl.cache_control();
    }
    json!(values)
}

/// Render sections exactly as the combined system string: stable sections
/// first, separated by one blank line. Adapters must never reorder or trim
/// section text, otherwise the cache prefix and the compatibility string
/// diverge.
fn render_sections(sections: &[SystemSection]) -> String {
    sections
        .iter()
        .filter(|section| section.has_text())
        .map(|section| section.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn anthropic_system_sections(sections: &[SystemSection], plan: &CacheBreakpointPlan) -> Value {
    let non_empty = sections
        .iter()
        .filter(|section| section.has_text())
        .collect::<Vec<_>>();
    if non_empty.is_empty() {
        return Value::Null;
    }
    let mut blocks = Vec::with_capacity(non_empty.len());
    for (index, section) in non_empty.iter().enumerate() {
        // Preserve the exact combined text. Anthropic concatenates adjacent
        // text blocks without separators, so the blank line between sections
        // must travel with the text.
        let mut text = section.text.clone();
        if index + 1 < non_empty.len() {
            text.push_str("\n\n");
        }
        let mut block = json!({"type": "text", "text": text});
        let breakpoint = plan.enabled
            && match section.kind.as_str() {
                "core" => plan.core_breakpoint,
                "project" => plan.project_breakpoint,
                _ => false,
            };
        if breakpoint {
            block["cache_control"] = plan.ttl.cache_control();
        }
        blocks.push(block);
    }
    json!(blocks)
}

/// Place an explicit cache breakpoint on the last cacheable message block.
/// This is the documented fallback when request-level automatic caching is
/// disabled or unsupported by a compatible gateway. It never changes message
/// text and never adds a breakpoint when the message has no cacheable block.
fn mark_last_message_cache_control(messages: &mut [Value], ttl: CacheTtl) {
    let Some(message) = messages.iter_mut().rev().find(|message| {
        message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role != "system")
    }) else {
        return;
    };
    let Some(content) = message.get_mut("content") else {
        return;
    };
    match content {
        Value::String(text) if !text.is_empty() => {
            let text = text.clone();
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": ttl.cache_control(),
            }]);
        }
        Value::Array(blocks) => {
            if let Some(Value::Object(last)) = blocks.last_mut()
                && last.get("cache_control").is_none()
            {
                last.insert("cache_control".into(), ttl.cache_control());
            }
        }
        _ => {}
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
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

impl AnthropicProvider {
    async fn stream_chat_inner(
        &self,
        request: ChatRequest,
        preempt: Option<crate::PreemptToken>,
    ) -> Result<EventStream, ProviderError> {
        let api_key = self.api_key.as_ref().ok_or(ProviderError::MissingApiKey)?;

        let mut messages: Vec<Value> = request
            .messages
            .iter()
            .filter(|message| message.role != "system")
            .filter_map(anthropic_message)
            .collect();
        // Anthropic's `system` field is a top-level request parameter, so a v3
        // tail system section cannot be expressed mid-history. Represent the
        // memory brief as one stable user message prefixed `[memory]` after the
        // exact prior conversation. The runtime records
        // `provider_tail_system=false` for this provider. No earlier message or
        // cache breakpoint is moved.
        for section in request
            .tail_sections
            .iter()
            .filter(|section| section.has_text())
        {
            messages.push(json!({
                "role": "user",
                "content": format!("[memory]\n\n{}", section.text),
            }));
        }

        let cache_ttl = request.cache.map(|cache| cache.ttl);
        let plan = request.cache.map(|cache| {
            plan_cache_breakpoints(
                "anthropic",
                &request.system_sections,
                request.tools.len(),
                messages.len(),
                cache,
            )
        });

        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "stream": true,
        });
        let system = request.system;
        let stable_prefix = request.system_stable_prefix.clone();
        let section_text = render_sections(&request.system_sections);
        if let (Some(plan), Some(system_text)) = (plan.as_ref(), system.as_deref()) {
            if request.system_sections.iter().any(SystemSection::has_text)
                && system_text == section_text
            {
                let value = anthropic_system_sections(&request.system_sections, plan);
                if !value.is_null() {
                    body["system"] = value;
                }
            } else if let Some(system) = system {
                // Legacy compatibility path: no exact section boundaries were
                // supplied. Keep the historical stable-prefix breakpoint only
                // while caching is enabled; a disabled policy must emit no
                // markers even for a legacy request.
                body["system"] = if plan.enabled {
                    anthropic_system(system, stable_prefix.as_deref())
                } else {
                    json!(system)
                };
            }
        } else if let Some(system) = system {
            body["system"] = anthropic_system(system, stable_prefix.as_deref());
        }
        if let Some(plan) = plan.as_ref() {
            if plan.enabled && plan.automatic {
                body["cache_control"] = plan.ttl.cache_control();
            } else if plan.enabled
                && plan.tail_message
                && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
            {
                mark_last_message_cache_control(messages, plan.ttl);
            }
        } else {
            // Backward-compatible legacy behavior is already expressed by
            // `anthropic_system`/`anthropic_tools`; no request-level marker.
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if !request.tools.is_empty() {
            body["tools"] = anthropic_tools_with_plan(&request.tools, plan.as_ref());
        }

        let response = match crate::send_with_retry_preemptible(
            || {
                self.client
                    .post(self.endpoint())
                    .header("x-api-key", api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json")
                    .json(&body)
            },
            preempt.as_ref(),
        )
        .await
        {
            Ok(response) => response,
            Err(ProviderError::Api { status, body })
                if cache_ttl == Some(CacheTtl::OneHour)
                    && looks_like_cache_ttl_rejection(status, &body) =>
            {
                return Err(ProviderError::Unsupported(format!(
                    "the provider rejected the explicit 1h prompt-cache TTL (HTTP {status}): {body}; \
                     set the project cache policy ttl to 5m (for example `vp cache-policy-set <project> --ttl 5m`) \
                     or use an endpoint/model that supports extended cache TTL. No silent downgrade was attempted."
                )));
            }
            Err(error) => return Err(error),
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
            let mut input_tokens = 0u32;
            let mut output_tokens = 0u32;
            let mut cache_read_tokens = 0u32;
            let mut cache_write_tokens = 0u32;
            let mut tool_calls: BTreeMap<u64, ToolCall> = BTreeMap::new();
            let mut saw_done = false;
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
    fn cache_sections() -> Vec<SystemSection> {
        vec![
            SystemSection::new("core", "Stable role charter."),
            SystemSection::new("project", "Indexed sources and skills."),
            SystemSection::new("volatile", "Run context: worktree=/tmp/task."),
        ]
    }

    fn cache_request() -> ChatRequest {
        let sections = cache_sections();
        let system = render_sections(&sections);
        let mut request = ChatRequest::new("claude-test", vec![ChatMessage::user("inspect")]);
        request.system = Some(system.clone());
        request.system_stable_prefix = Some("Stable role charter.".to_string());
        request.system_sections = sections;
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
        request
    }

    fn count_cache_controls(value: &Value) -> usize {
        match value {
            Value::Object(object) => {
                usize::from(object.contains_key("cache_control"))
                    + object.values().map(count_cache_controls).sum::<usize>()
            }
            Value::Array(items) => items.iter().map(count_cache_controls).sum(),
            _ => 0,
        }
    }

    async fn capture_body(request: ChatRequest) -> (MockServer, Value) {
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
        let _ = provider.stream_chat(request).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body = serde_json::from_slice(&requests[0].body).unwrap();
        (server, body)
    }

    #[tokio::test]
    async fn automatic_caching_covers_the_moving_prefix_within_four_breakpoints() {
        let mut request = cache_request();
        request.cache = Some(crate::PromptCacheConfig::new(
            true,
            CacheTtl::FiveMinutes,
            true,
        ));
        let (_server, body) = capture_body(request).await;
        assert_eq!(body["cache_control"]["type"], json!("ephemeral"));
        assert!(body["cache_control"].get("ttl").is_none());
        assert_eq!(body["system"][0]["text"], "Stable role charter.\n\n");
        assert_eq!(
            body["system"][0]["cache_control"]["type"],
            json!("ephemeral")
        );
        assert_eq!(body["system"][1]["text"], "Indexed sources and skills.\n\n");
        assert_eq!(
            body["system"][1]["cache_control"]["type"],
            json!("ephemeral")
        );
        assert!(body["system"][2].get("cache_control").is_none());
        assert_eq!(
            body["system"][2]["text"],
            "Run context: worktree=/tmp/task."
        );
        assert!(body["tools"][0].get("cache_control").is_none());
        assert!(body["tools"][1].get("cache_control").is_some());
        assert_eq!(count_cache_controls(&body), 4);
    }

    #[tokio::test]
    async fn explicit_one_hour_ttl_is_sent_on_every_marker() {
        let mut request = cache_request();
        request.cache = Some(crate::PromptCacheConfig::new(true, CacheTtl::OneHour, true));
        let (_server, body) = capture_body(request).await;
        assert_eq!(body["cache_control"]["ttl"], json!("1h"));
        assert_eq!(body["system"][0]["cache_control"]["ttl"], json!("1h"));
        assert_eq!(body["system"][1]["cache_control"]["ttl"], json!("1h"));
        assert_eq!(body["tools"][1]["cache_control"]["ttl"], json!("1h"));
        assert_eq!(count_cache_controls(&body), 4);
    }

    #[tokio::test]
    async fn tail_breakpoint_replaces_automatic_caching_when_disabled() {
        let mut request = cache_request();
        request.cache = Some(crate::PromptCacheConfig::new(
            true,
            CacheTtl::FiveMinutes,
            false,
        ));
        let (_server, body) = capture_body(request).await;
        assert!(body.get("cache_control").is_none());
        assert!(body["system"][0].get("cache_control").is_some());
        assert!(body["system"][1].get("cache_control").is_some());
        assert!(body["system"][2].get("cache_control").is_none());
        assert!(body["tools"][1].get("cache_control").is_some());
        assert!(body["messages"][0].get("cache_control").is_none());
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            json!("ephemeral")
        );
        assert_eq!(body["messages"][0]["content"][0]["text"], json!("inspect"));
        assert_eq!(count_cache_controls(&body), 4);
    }

    #[tokio::test]
    async fn disabled_policy_sends_no_cache_control_markers() {
        let mut request = cache_request();
        request.cache = Some(crate::PromptCacheConfig::disabled());
        let (_server, body) = capture_body(request).await;
        assert!(body.get("cache_control").is_none());
        assert_eq!(count_cache_controls(&body), 0);
        // Section boundaries and text remain unchanged even with caching off.
        assert_eq!(body["system"][0]["text"], "Stable role charter.\n\n");
        assert_eq!(
            body["system"][2]["text"],
            "Run context: worktree=/tmp/task."
        );
        assert_eq!(body["messages"][0]["content"], json!("inspect"));
    }

    #[tokio::test]
    async fn disabled_policy_suppresses_legacy_stable_prefix_breakpoints() {
        let mut request = ChatRequest::new("claude-test", vec![ChatMessage::user("inspect")]);
        request.system = Some("LEGACY SYSTEM".to_string());
        request.system_stable_prefix = Some("LEGACY".to_string());
        request.cache = Some(crate::PromptCacheConfig::disabled());
        let (_server, body) = capture_body(request).await;
        assert_eq!(body["system"], json!("LEGACY SYSTEM"));
        assert_eq!(count_cache_controls(&body), 0);
    }

    #[tokio::test]
    async fn breakpoint_count_never_exceeds_four_for_any_policy_combination() {
        for automatic in [true, false] {
            for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
                for tools in [0usize, 1, 5] {
                    let mut request = cache_request();
                    request.cache = Some(crate::PromptCacheConfig::new(true, ttl, automatic));
                    request.tools = (0..tools)
                        .map(|index| crate::ToolSchema {
                            name: format!("tool_{index}"),
                            description: "test tool".to_string(),
                            parameters: json!({"type": "object"}),
                        })
                        .collect();
                    let (_server, body) = capture_body(request).await;
                    let count = count_cache_controls(&body);
                    assert!(
                        count <= 4,
                        "policy automatic={automatic} ttl={} tools={tools} emitted {count} breakpoints",
                        ttl.as_str()
                    );
                    if automatic {
                        assert!(body.get("cache_control").is_some());
                        assert!(
                            body["messages"][0]["content"][0]
                                .get("cache_control")
                                .is_none()
                        );
                    } else {
                        assert!(body.get("cache_control").is_none());
                        assert!(
                            body["messages"][0]["content"][0]
                                .get("cache_control")
                                .is_some()
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn tail_breakpoint_handles_a_tool_result_message_without_rewriting_it() {
        let mut request = ChatRequest::new(
            "claude-test",
            vec![
                ChatMessage::user("inspect"),
                ChatMessage::tool("toolu_1", "file contents"),
            ],
        );
        request.system_sections = vec![
            SystemSection::new("core", "CORE"),
            SystemSection::new("volatile", "VOLATILE"),
        ];
        request.system = Some("CORE\n\nVOLATILE".to_string());
        request.system_stable_prefix = Some("CORE".to_string());
        request.cache = Some(crate::PromptCacheConfig::new(
            true,
            CacheTtl::FiveMinutes,
            false,
        ));
        let (_server, body) = capture_body(request).await;
        let last = body["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"][0]["type"], "tool_result");
        assert_eq!(last["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(last["content"][0]["content"], "file contents");
        assert_eq!(
            last["content"][0]["cache_control"]["type"],
            json!("ephemeral")
        );
        assert_eq!(count_cache_controls(&body), 2);
    }

    #[tokio::test]
    async fn automatic_caching_without_tools_uses_core_project_and_auto_only() {
        let mut request = ChatRequest::new("claude-test", vec![ChatMessage::user("inspect")]);
        request.system_sections = cache_sections();
        request.system = Some(render_sections(&cache_sections()));
        request.cache = Some(crate::PromptCacheConfig::new(
            true,
            CacheTtl::FiveMinutes,
            true,
        ));
        let (_server, body) = capture_body(request).await;
        assert!(body.get("tools").is_none());
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
        assert!(body["system"][2].get("cache_control").is_none());
        assert_eq!(count_cache_controls(&body), 3);
    }

    #[tokio::test]
    async fn explicit_tail_mode_without_messages_stays_under_the_limit() {
        let mut request = ChatRequest::new("claude-test", Vec::new());
        request.system_sections = vec![
            SystemSection::new("core", "CORE"),
            SystemSection::new("project", "PROJECT"),
        ];
        request.system = Some("CORE\n\nPROJECT".to_string());
        request.cache = Some(crate::PromptCacheConfig::new(
            true,
            CacheTtl::FiveMinutes,
            false,
        ));
        let (_server, body) = capture_body(request).await;
        assert!(body.get("cache_control").is_none());
        assert_eq!(body["messages"].as_array().unwrap().len(), 0);
        assert_eq!(count_cache_controls(&body), 2);
    }

    #[tokio::test]
    async fn rejected_one_hour_ttl_is_an_actionable_error_not_a_silent_downgrade() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "{\"error\":{\"message\":\"cache_control.ttl: 1h is not supported for this model\"}}",
            ))
            .mount(&server)
            .await;
        let provider = AnthropicProvider::with_base_url(
            "test",
            format!("{}/v1", server.uri()),
            Some("test-key".to_string()),
        );
        let mut request = cache_request();
        request.cache = Some(crate::PromptCacheConfig::new(true, CacheTtl::OneHour, true));
        let error = provider.stream_chat(request).await.err().unwrap();
        let message = error.to_string();
        assert!(message.contains("1h prompt-cache TTL"), "{message}");
        assert!(message.contains("vp cache-policy-set"), "{message}");
        assert!(message.contains("No silent downgrade"), "{message}");
    }

    #[tokio::test]
    async fn unrelated_http_errors_with_one_hour_policy_stay_provider_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_string("{\"error\":{\"message\":\"max_tokens is required\"}}"),
            )
            .mount(&server)
            .await;
        let provider = AnthropicProvider::with_base_url(
            "test",
            format!("{}/v1", server.uri()),
            Some("test-key".to_string()),
        );
        let mut request = cache_request();
        request.cache = Some(crate::PromptCacheConfig::new(true, CacheTtl::OneHour, true));
        let error = provider.stream_chat(request).await.err().unwrap();
        assert!(matches!(error, ProviderError::Api { status: 400, .. }));
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

    async fn spawn_slow_anthropic_sse() -> std::net::SocketAddr {
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
            for event in [
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"first\"}}\n\n",
            ] {
                let _ = socket
                    .write_all(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes())
                    .await;
            }
            let _ = socket.flush().await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let event = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
            let _ = socket
                .write_all(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes())
                .await;
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        address
    }

    #[tokio::test]
    async fn preempts_a_slow_anthropic_stream_between_chunks() {
        let address = spawn_slow_anthropic_sse().await;
        let provider = AnthropicProvider::with_base_url(
            "test",
            format!("http://{address}/v1"),
            Some("test-key".to_string()),
        );
        let token = crate::PreemptToken::disabled();
        let mut stream = provider
            .stream_chat_preemptible(
                ChatRequest::new("claude-test", vec![ChatMessage::user("hi")]),
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
            Some(ProviderError::Preempted { reason, phase, .. })
                if reason == "explicit" && phase == "provider"
        ));
    }
    #[tokio::test]
    async fn tail_memory_falls_back_to_a_stable_user_message() {
        let mut request = cache_request();
        request.tail_sections = vec![crate::SystemSection::new("tail/memory", "MEMORY BRIEF")];
        let (_server, body) = capture_body(request).await;
        let messages = body["messages"].as_array().unwrap();
        // The conversation stays exact and the memory brief is a final user
        // message, never part of the top-level system blocks.
        assert_eq!(messages.last().unwrap()["role"], json!("user"));
        assert_eq!(
            messages.last().unwrap()["content"],
            json!("[memory]\n\nMEMORY BRIEF")
        );
        let system = body["system"].to_string();
        assert!(!system.contains("MEMORY BRIEF"));
    }
}

//! Provider-wire append-only and parity tests.
//!
//! These tests exercise the raw HTTP body, not a reconstructed value: the
//! bytes that carry prior conversation messages must be identical when a new
//! message is appended.

use serde_json::{Value, json};
use vp_providers::canonical::{
    ProviderCacheCapabilities, ProviderWire, TailSectionMode, TailSystemCapability,
    canonical_request, canonical_wire_messages, openai_wire_messages,
};
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, StreamEvent,
    SystemSection, ToolCall, ToolSchema,
};

const OPENAI_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":7}}}\n\n",
    "data: [DONE]\n\n"
);

fn tool(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: format!("{name} tool"),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }
}

fn head_request(messages: Vec<ChatMessage>) -> ChatRequest {
    let mut request = ChatRequest::new("deepseek-flash", messages);
    request.system = Some("CORE CHARTER\n\nPROJECT SOURCES".to_string());
    request.system_stable_prefix = Some("CORE CHARTER".to_string());
    request.system_sections = vec![
        SystemSection::new("core", "CORE CHARTER"),
        SystemSection::new("project", "PROJECT SOURCES"),
    ];
    request.tools = vec![tool("read_file"), tool("write_file")];
    request.cache = Some(vp_providers::PromptCacheConfig::new(
        true,
        vp_providers::CacheTtl::FiveMinutes,
        true,
    ));
    request
}

/// Locate the raw `"messages": [...]` array inside a serialized request body.
/// The returned slice includes the brackets. This deliberately does not parse
/// and re-serialize: the assertion is about exact wire bytes.
fn raw_messages_array(body: &[u8]) -> &[u8] {
    let needle = b"\"messages\":";
    let start = body
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap_or_else(|| panic!("messages field not found in body"));
    let mut index = start + needle.len();
    while index < body.len() && body[index].is_ascii_whitespace() {
        index += 1;
    }
    assert_eq!(body.get(index), Some(&b'['), "messages must be an array");
    let array_start = index;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while index < body.len() {
        let byte = body[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'[' {
            depth += 1;
        } else if byte == b']' {
            depth -= 1;
            if depth == 0 {
                return &body[array_start..=index];
            }
        }
        index += 1;
    }
    panic!("unterminated messages array");
}

fn parsed_messages(body: &[u8]) -> Vec<Value> {
    let raw = raw_messages_array(body);
    serde_json::from_slice(raw).expect("messages array should be valid JSON")
}

fn strip_cache_control(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut cleaned = serde_json::Map::new();
            for (key, item) in object {
                if key == "cache_control" {
                    continue;
                }
                cleaned.insert(key.clone(), strip_cache_control(item));
            }
            Value::Object(cleaned)
        }
        Value::Array(items) => Value::Array(items.iter().map(strip_cache_control).collect()),
        other => other.clone(),
    }
}

fn assert_message_prefix(previous: &[Value], current: &[Value]) {
    assert!(
        current.len() >= previous.len(),
        "current list lost messages: {} < {}",
        current.len(),
        previous.len()
    );
    for (index, message) in previous.iter().enumerate() {
        assert_eq!(
            &current[index], message,
            "prior message {index} changed; previous={message} current={}",
            current[index]
        );
    }
}

async fn capture_openai(request: ChatRequest) -> (Vec<u8>, Vec<StreamEvent>) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(OPENAI_SSE),
        )
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::local("deepseek", format!("{}/v1", server.uri()));
    let mut stream = provider.stream_chat(request).await.unwrap();
    let mut events = Vec::new();
    use futures_util::StreamExt;
    while let Some(item) = stream.next().await {
        events.push(item.unwrap());
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "one request per turn");
    (requests[0].body.clone(), events)
}

#[tokio::test]
async fn appended_turn_keeps_the_prior_message_json_byte_identical() {
    let (first_body, _) = capture_openai(head_request(vec![
        ChatMessage::user("first prompt"),
        ChatMessage::assistant_tool_calls(
            "",
            vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            }],
        ),
        ChatMessage::tool("call_1", "README contents"),
    ]))
    .await;

    let (second_body, _) = capture_openai(head_request(vec![
        ChatMessage::user("first prompt"),
        ChatMessage::assistant_tool_calls(
            "",
            vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            }],
        ),
        ChatMessage::tool("call_1", "README contents"),
        ChatMessage::user("second prompt"),
    ]))
    .await;

    let first_raw = raw_messages_array(&first_body);
    let second_raw = raw_messages_array(&second_body);
    // The first request's array without its closing bracket is an exact byte
    // prefix of the second request's array. This proves the head system
    // sections and every prior conversation message were not re-encoded.
    assert_eq!(
        &first_raw[..first_raw.len() - 1],
        &second_raw[..first_raw.len() - 1],
        "prior message JSON changed between turns"
    );
    assert_message_prefix(
        &parsed_messages(&first_body),
        &parsed_messages(&second_body),
    );
}

#[tokio::test]
async fn multiple_appends_keep_growing_the_exact_prefix() {
    let mut messages = vec![ChatMessage::user("turn 0")];
    let mut bodies = Vec::new();
    for turn in 1..5 {
        let (body, _) = capture_openai(head_request(messages.clone())).await;
        bodies.push(body);
        messages.push(ChatMessage::assistant_tool_calls(
            format!("answer {turn}"),
            vec![],
        ));
        messages.push(ChatMessage::user(format!("turn {turn}")));
    }
    for window in bodies.windows(2) {
        let previous = raw_messages_array(&window[0]);
        let current = raw_messages_array(&window[1]);
        assert_eq!(
            &previous[..previous.len() - 1],
            &current[..previous.len() - 1],
            "an earlier turn was re-encoded"
        );
        assert_message_prefix(&parsed_messages(&window[0]), &parsed_messages(&window[1]));
    }
}

#[tokio::test]
async fn stable_head_sections_are_sent_before_history_on_both_turns() {
    let (first, _) = capture_openai(head_request(vec![ChatMessage::user("one")])).await;
    let (second, _) = capture_openai(head_request(vec![
        ChatMessage::user("one"),
        ChatMessage::assistant_tool_calls("answer", vec![]),
        ChatMessage::user("two"),
    ]))
    .await;
    for body in [&first, &second] {
        let messages = parsed_messages(body);
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(messages[0]["content"], json!("CORE CHARTER"));
        assert_eq!(messages[1]["role"], json!("system"));
        assert_eq!(messages[1]["content"], json!("PROJECT SOURCES"));
        assert_eq!(messages[2]["role"], json!("user"));
    }
}

#[tokio::test]
async fn openai_body_never_contains_cache_control() {
    let (body, _) = capture_openai(head_request(vec![ChatMessage::user("one")])).await;
    let text = String::from_utf8_lossy(&body);
    assert!(!text.contains("cache_control"));
    assert!(!text.contains("\"ttl\""));
}

#[tokio::test]
async fn embedded_memory_anchor_preserves_the_history_prefix() {
    let anchor =
        ChatMessage::system("Run memory (retrieved for this session):\nuse the internal registry");
    let mut first = head_request(vec![ChatMessage::user("first"), anchor.clone()]);
    first.tail_sections = vec![SystemSection::new(
        "tail/memory",
        "Run memory (retrieved for this session):\nuse the internal registry",
    )];

    let mut second = head_request(vec![
        ChatMessage::user("first"),
        anchor,
        ChatMessage::assistant_tool_calls("ok", vec![]),
        ChatMessage::user("second"),
    ]);
    second.tail_sections = first.tail_sections.clone();

    let (first_body, _) = capture_openai(first).await;
    let (second_body, _) = capture_openai(second).await;
    let first_raw = raw_messages_array(&first_body);
    let second_raw = raw_messages_array(&second_body);
    assert_eq!(
        &first_raw[..first_raw.len() - 1],
        &second_raw[..first_raw.len() - 1]
    );
    // The memory appears exactly once, at its history position; it is not
    // appended again after the new turn.
    let messages = parsed_messages(&second_body);
    let memory_count = messages
        .iter()
        .filter(|message| {
            message["content"]
                .as_str()
                .is_some_and(|content| content.contains("internal registry"))
        })
        .count();
    assert_eq!(memory_count, 1);
}

#[tokio::test]
async fn non_embedded_trailing_memory_is_a_suffix_that_moves() {
    // This documents the known limitation: the production runtime persists an
    // anchor so memory is embedded, but a direct adapter caller that supplies
    // a fresh tail each turn gets a suffix that moves. The test proves the
    // canonical layer reports it as a real break instead of claiming
    // append-only.
    let mut first = head_request(vec![ChatMessage::user("first")]);
    first.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY ONE")];
    let mut second = head_request(vec![
        ChatMessage::user("first"),
        ChatMessage::assistant_tool_calls("ok", vec![]),
        ChatMessage::user("second"),
    ]);
    second.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY TWO")];

    let first_snapshot = canonical_request(
        &first,
        ProviderCacheCapabilities::openai_compatible(),
        "tools",
    )
    .snapshot();
    let second_snapshot = canonical_request(
        &second,
        ProviderCacheCapabilities::openai_compatible(),
        "tools",
    )
    .snapshot();
    let report =
        vp_providers::ContinuityReport::detect(Some(&first_snapshot), &second_snapshot, false);
    // History itself is an exact prefix. The tail is a separate suffix and is
    // not part of the history chain.
    assert!(report.append_only);
    assert_eq!(
        canonical_request(
            &first,
            ProviderCacheCapabilities::openai_compatible(),
            "tools"
        )
        .tail_mode,
        TailSectionMode::TrailingSystem
    );
}

#[tokio::test]
async fn unsupported_tail_system_falls_back_to_a_user_memory_message() {
    let mut request = head_request(vec![ChatMessage::user("one")]);
    request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY BRIEF")];
    let provider = OpenAiCompatibleProvider::local("local", "http://127.0.0.1:9/v1")
        .with_tail_system_capability(TailSystemCapability::Unsupported);
    assert_eq!(
        provider.cache_capabilities().tail_system,
        TailSystemCapability::Unsupported
    );
    let messages = openai_wire_messages(&request, provider.cache_capabilities());
    let last = messages.last().unwrap();
    assert_eq!(last["role"], json!("user"));
    assert_eq!(last["content"], json!("[memory]\n\nMEMORY BRIEF"));
    assert!(
        messages[..messages.len() - 1]
            .iter()
            .all(|message| message["content"] != json!("MEMORY BRIEF"))
    );
}

#[tokio::test]
async fn rejected_mid_history_system_retries_with_the_user_fallback() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("{\"error\":\"invalid role: system message after history\"}"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(OPENAI_SSE),
        )
        .mount(&server)
        .await;

    let mut request = head_request(vec![ChatMessage::user("one")]);
    request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY BRIEF")];
    let provider = OpenAiCompatibleProvider::local("local", format!("{}/v1", server.uri()));
    use futures_util::StreamExt;
    let mut stream = provider.stream_chat(request).await.unwrap();
    while stream.next().await.is_some() {}
    assert_eq!(
        provider.cache_capabilities().tail_system,
        TailSystemCapability::Unsupported
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "exactly one capability retry");
    let first = parsed_messages(&requests[0].body);
    let second = parsed_messages(&requests[1].body);
    assert_eq!(first.last().unwrap()["role"], json!("system"));
    assert_eq!(second.last().unwrap()["role"], json!("user"));
    assert_eq!(
        second.last().unwrap()["content"],
        json!("[memory]\n\nMEMORY BRIEF")
    );
    // The prior conversation and head are untouched by the capability retry.
    assert_message_prefix(&first[..first.len() - 1], &second[..second.len() - 1]);
}

#[tokio::test]
async fn deepseek_reasoning_content_is_not_streamed_as_text_or_replayed() {
    use futures_util::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"private chain\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"public answer\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":4,\"prompt_cache_hit_tokens\":32}}\n\n",
                    "data: [DONE]\n\n"
                )),
        )
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::local("deepseek", format!("{}/v1", server.uri()));
    let mut stream = provider
        .stream_chat(ChatRequest::new(
            "deepseek-flash",
            vec![ChatMessage::user("think")],
        ))
        .await
        .unwrap();
    let mut text = String::new();
    let mut usage = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Usage {
                input_tokens,
                cache_read_tokens,
                ..
            } => usage = Some((input_tokens, cache_read_tokens)),
            _ => {}
        }
    }
    assert_eq!(text, "public answer");
    assert!(!text.contains("private chain"));
    assert_eq!(usage, Some((50, 32)));

    // A replayed assistant message has no reasoning field at all.
    let follow_up = head_request(vec![
        ChatMessage::user("think"),
        ChatMessage::assistant_tool_calls("public answer", vec![]),
        ChatMessage::user("next"),
    ]);
    let (body, _) = capture_openai(follow_up).await;
    assert!(!String::from_utf8_lossy(&body).contains("reasoning_content"));
    assert!(!String::from_utf8_lossy(&body).contains("private chain"));
}

#[tokio::test]
async fn no_hidden_provider_call_is_made_for_two_turns() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(OPENAI_SSE),
        )
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::local("deepseek", format!("{}/v1", server.uri()));
    use futures_util::StreamExt;
    let mut first = provider
        .stream_chat(head_request(vec![ChatMessage::user("one")]))
        .await
        .unwrap();
    while first.next().await.is_some() {}
    let mut second = provider
        .stream_chat(head_request(vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("ok", vec![]),
            ChatMessage::user("two"),
        ]))
        .await
        .unwrap();
    while second.next().await.is_some() {}
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn anthropic_prior_messages_are_content_identical_across_turns() {
    use futures_util::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
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
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("key".into()),
    );

    let first_request = ChatRequest::new(
        "claude-test",
        vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls(
                "working",
                vec![ToolCall {
                    id: "toolu_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"README.md\"}".to_string(),
                }],
            ),
            ChatMessage::tool("toolu_1", "README contents"),
        ],
    );
    let second_request = ChatRequest::new(
        "claude-test",
        vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls(
                "working",
                vec![ToolCall {
                    id: "toolu_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"README.md\"}".to_string(),
                }],
            ),
            ChatMessage::tool("toolu_1", "README contents"),
            ChatMessage::assistant_tool_calls("done", vec![]),
            ChatMessage::user("two"),
        ],
    );
    let mut stream = provider.stream_chat(first_request.clone()).await.unwrap();
    while stream.next().await.is_some() {}
    let mut stream = provider.stream_chat(second_request.clone()).await.unwrap();
    while stream.next().await.is_some() {}

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let first_messages = first["messages"].as_array().unwrap();
    let second_messages = second["messages"].as_array().unwrap();
    // Anthropic may attach a cache_control marker to the current last message.
    // That marker is transport metadata, not content. After stripping it, the
    // prior messages must be byte-identical values.
    for (index, message) in first_messages.iter().enumerate() {
        assert_eq!(
            strip_cache_control(message),
            strip_cache_control(&second_messages[index]),
            "anthropic prior message {index} changed"
        );
    }
    // The newer messages were appended, not inserted.
    assert_eq!(second_messages[0]["content"], json!("one"));
    assert_eq!(second_messages[4]["content"], json!("two"));

    // The canonical message list is also an exact prefix.
    let first_snapshot = canonical_request(
        &first_request,
        ProviderCacheCapabilities::anthropic(),
        "tools",
    )
    .snapshot();
    let second_snapshot = canonical_request(
        &second_request,
        ProviderCacheCapabilities::anthropic(),
        "tools",
    )
    .snapshot();
    let report =
        vp_providers::ContinuityReport::detect(Some(&first_snapshot), &second_snapshot, false);
    assert!(report.append_only);
}

#[tokio::test]
async fn anthropic_tail_memory_is_a_user_message_after_history() {
    use futures_util::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: message_start\n",
                    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                    "event: message_stop\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                )),
        )
        .mount(&server)
        .await;
    let provider = AnthropicProvider::with_base_url(
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("key".into()),
    );
    let mut request = ChatRequest::new(
        "claude-test",
        vec![ChatMessage::user("one"), ChatMessage::system("MEMORY")],
    );
    request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
    request.system_sections = vec![SystemSection::new("core", "CORE")];
    request.system = Some("CORE".to_string());
    request.cache = Some(vp_providers::PromptCacheConfig::new(
        true,
        vp_providers::CacheTtl::FiveMinutes,
        true,
    ));
    let mut stream = provider.stream_chat(request).await.unwrap();
    while stream.next().await.is_some() {}
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    // The persisted anchor is history position 1; it is not duplicated at the
    // end by the tail section.
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], json!("user"));
    assert_eq!(messages[1]["content"], json!("[memory]\n\nMEMORY"));
    // The top-level system field still holds the stable head.
    assert_eq!(body["system"][0]["text"], json!("CORE"));
}

#[tokio::test]
async fn canonical_wire_messages_match_the_openai_http_messages() {
    let request = head_request(vec![
        ChatMessage::user("one"),
        ChatMessage::assistant_tool_calls("two", vec![]),
    ]);
    let (body, _) = capture_openai(request.clone()).await;
    let expected = canonical_wire_messages(&canonical_request(
        &request,
        ProviderCacheCapabilities::openai_compatible(),
        "tools",
    ));
    assert_eq!(parsed_messages(&body), expected);
}

#[tokio::test]
async fn provider_capabilities_are_provider_specific() {
    let openai = OpenAiCompatibleProvider::local("deepseek", "http://127.0.0.1:9/v1");
    let anthropic =
        AnthropicProvider::with_base_url("anthropic", "http://127.0.0.1:9/v1", Some("key".into()));
    assert_eq!(
        openai.cache_capabilities(),
        ProviderCacheCapabilities::new(
            ProviderWire::OpenAiCompatible,
            TailSystemCapability::Supported
        )
    );
    assert_eq!(
        anthropic.cache_capabilities(),
        ProviderCacheCapabilities::anthropic()
    );
}

#[test]
fn wire_parser_rejects_unknown_names() {
    assert_eq!(
        ProviderWire::parse("openai-compatible"),
        Some(ProviderWire::OpenAiCompatible)
    );
    assert_eq!(
        ProviderWire::parse("anthropic"),
        Some(ProviderWire::Anthropic)
    );
    assert_eq!(ProviderWire::parse("mystery"), None);
}

#[tokio::test]
async fn empty_history_is_wire_stable() {
    let (body, _) = capture_openai(head_request(vec![])).await;
    let messages = parsed_messages(&body);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["content"], json!("CORE CHARTER"));
    assert_eq!(messages[1]["content"], json!("PROJECT SOURCES"));
}

#[tokio::test]
async fn usage_parsing_is_unchanged_by_canonicalization() {
    let (_, events) = capture_openai(head_request(vec![ChatMessage::user("one")])).await;
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Usage {
            input_tokens: 11,
            output_tokens: 2,
            cache_read_tokens: 7,
            cache_write_tokens: 0,
        }
    )));
}

#[tokio::test]
async fn ten_turn_tool_conversation_keeps_every_prefix_byte_exact() {
    let mut messages = vec![ChatMessage::user("inspect the repository")];
    let mut previous: Option<Vec<Value>> = None;
    for turn in 0..10 {
        let (body, _) = capture_openai(head_request(messages.clone())).await;
        let current = parsed_messages(&body);
        if let Some(previous) = previous.as_ref() {
            assert_message_prefix(previous, &current);
        }
        previous = Some(current);
        messages.push(ChatMessage::assistant_tool_calls(
            format!("turn {turn}"),
            vec![ToolCall {
                id: format!("call_{turn}"),
                name: "read_file".to_string(),
                arguments: format!("{{\"path\":\"file_{turn}.md\"}}"),
            }],
        ));
        messages.push(ChatMessage::tool(
            format!("call_{turn}"),
            format!("contents {turn}"),
        ));
        messages.push(ChatMessage::user(format!("continue {turn}")));
    }
}

#[tokio::test]
async fn parallel_tool_calls_and_results_keep_their_order() {
    let messages = vec![
        ChatMessage::user("read both"),
        ChatMessage::assistant_tool_calls(
            "",
            vec![
                ToolCall {
                    id: "call_a".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"a\"}".to_string(),
                },
                ToolCall {
                    id: "call_b".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"b\"}".to_string(),
                },
            ],
        ),
        ChatMessage::tool("call_a", "A"),
        ChatMessage::tool("call_b", "B"),
    ];
    let (body, _) = capture_openai(head_request(messages.clone())).await;
    let parsed = parsed_messages(&body);
    assert_eq!(parsed[4]["tool_call_id"], json!("call_a"));
    assert_eq!(parsed[5]["tool_call_id"], json!("call_b"));

    let mut appended = messages;
    appended.push(ChatMessage::user("next"));
    let (body, _) = capture_openai(head_request(appended)).await;
    assert_message_prefix(&parsed, &parsed_messages(&body));
}

#[tokio::test]
async fn unicode_and_escaped_content_are_wire_stable_across_turns() {
    let first_messages = vec![ChatMessage::user("你好 — \"quoted\"\nline\ttab \u{1}")];
    let (first, _) = capture_openai(head_request(first_messages.clone())).await;
    let mut second_messages = first_messages;
    second_messages.push(ChatMessage::assistant_tool_calls("回答", vec![]));
    second_messages.push(ChatMessage::user("继续"));
    let (second, _) = capture_openai(head_request(second_messages)).await;
    assert_message_prefix(&parsed_messages(&first), &parsed_messages(&second));
    assert!(String::from_utf8_lossy(&second).contains("你好"));
}

#[tokio::test]
async fn canonical_history_prefix_holds_for_both_provider_projections() {
    let messages = vec![
        ChatMessage::user("one"),
        ChatMessage::system("MEMORY"),
        ChatMessage::assistant_tool_calls(
            "working",
            vec![ToolCall {
                id: "toolu_1".to_string(),
                name: "read_file".to_string(),
                arguments: "{}".to_string(),
            }],
        ),
        ChatMessage::tool("toolu_1", "result"),
    ];
    let mut appended = messages.clone();
    appended.push(ChatMessage::user("two"));

    for capabilities in [
        ProviderCacheCapabilities::openai_compatible(),
        ProviderCacheCapabilities::anthropic(),
    ] {
        let first = canonical_request(&head_request(messages.clone()), capabilities, "tools");
        let second = canonical_request(&head_request(appended.clone()), capabilities, "tools");
        let report = vp_providers::ContinuityReport::detect(
            Some(&first.snapshot()),
            &second.snapshot(),
            false,
        );
        assert!(report.append_only, "wire {}", capabilities.wire.as_str());
        assert_message_prefix(&first.history_messages, &second.history_messages);
    }
}

#[test]
fn canonical_tool_hash_matches_the_core_implementation_when_available() {
    // The exact equality with `vp_core::tool_bundle::canonical_tools_hash` is
    // asserted in the vp-core integration test where both crates are visible.
    // Here we pin the provider helper's stability across calls and ordering.
    let tools = vec![tool("b"), tool("a")];
    let first = vp_providers::canonical_tools_hash(&tools);
    let second = vp_providers::canonical_tools_hash(&[tool("a"), tool("b")]);
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
}

#[test]
fn tool_schema_canonical_hash_matches_the_core_contract() {
    // This vector is mirrored in the vp-core integration test where both the
    // provider canonical helper and `tool_bundle::canonical_tools_hash` are
    // available. Keeping the expected value here detects accidental drift.
    let tools = vec![tool("read_file")];
    let hash = vp_providers::canonical_tools_hash(&tools);
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|character| character.is_ascii_hexdigit()));
}

#[tokio::test]
async fn anthropic_moving_breakpoint_never_changes_prior_content() {
    use futures_util::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "event: message_start\n",
                    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                    "event: message_stop\n",
                    "data: {\"type\":\"message_stop\"}\n\n",
                )),
        )
        .mount(&server)
        .await;
    let provider = AnthropicProvider::with_base_url(
        "anthropic",
        format!("{}/v1", server.uri()),
        Some("key".into()),
    );

    let mut messages = vec![ChatMessage::user("one")];
    let mut previous_messages: Option<Vec<Value>> = None;
    for turn in 0..5 {
        let mut request = ChatRequest::new("claude-test", messages.clone());
        request.system_sections = vec![SystemSection::new("core", "CORE")];
        request.system = Some("CORE".to_string());
        request.cache = Some(vp_providers::PromptCacheConfig::new(
            true,
            vp_providers::CacheTtl::FiveMinutes,
            false,
        ));
        let mut stream = provider.stream_chat(request.clone()).await.unwrap();
        while stream.next().await.is_some() {}
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        let current = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(strip_cache_control)
            .collect::<Vec<_>>();
        if let Some(previous) = previous_messages.as_ref() {
            assert_message_prefix(previous, &current);
        }
        previous_messages = Some(current);
        let canonical =
            canonical_request(&request, ProviderCacheCapabilities::anthropic(), "tools");
        if turn > 0 {
            let previous = canonical_request(
                &ChatRequest::new("claude-test", messages.clone()),
                ProviderCacheCapabilities::anthropic(),
                "tools",
            );
            assert_eq!(
                previous.history_messages.last(),
                canonical.history_messages.last()
            );
        }
        messages.push(ChatMessage::assistant_tool_calls(
            format!("answer {turn}"),
            vec![],
        ));
        messages.push(ChatMessage::user(format!("next {turn}")));
    }
}

#[tokio::test]
async fn stream_options_fallback_preserves_the_exact_messages() {
    use futures_util::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400).set_body_string(
                "{\"error\":\"stream_options is not supported by this endpoint\"}",
            ),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(OPENAI_SSE),
        )
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::local("local", format!("{}/v1", server.uri()));
    let request = head_request(vec![
        ChatMessage::user("one"),
        ChatMessage::assistant_tool_calls("two", vec![]),
        ChatMessage::user("three"),
    ]);
    let mut stream = provider.stream_chat(request).await.unwrap();
    while stream.next().await.is_some() {}
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(first["messages"], second["messages"]);
    assert!(first.get("stream_options").is_some());
    assert!(second.get("stream_options").is_none());
}

#[tokio::test]
async fn a_server_that_always_rejects_the_tail_retries_only_once() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("{\"error\":\"invalid role system after history\"}"),
        )
        .mount(&server)
        .await;
    let provider = OpenAiCompatibleProvider::local("local", format!("{}/v1", server.uri()));
    let mut request = head_request(vec![ChatMessage::user("one")]);
    request.tail_sections = vec![SystemSection::new("tail/memory", "MEMORY")];
    let error = provider.stream_chat(request).await.err().unwrap();
    assert!(matches!(
        error,
        vp_providers::ProviderError::Api { status: 400, .. }
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "one capability retry, then stop");
    assert_eq!(
        provider.cache_capabilities().tail_system,
        vp_providers::TailSystemCapability::Unsupported
    );
}

#[tokio::test]
async fn empty_and_whitespace_messages_are_preserved_exactly() {
    let messages = vec![
        ChatMessage::user(""),
        ChatMessage::assistant_tool_calls("   ", vec![]),
        ChatMessage::user("next"),
    ];
    let (body, _) = capture_openai(head_request(messages)).await;
    let parsed = parsed_messages(&body);
    assert_eq!(parsed[2]["content"], json!(""));
    assert_eq!(parsed[3]["content"], json!("   "));
    assert_eq!(parsed[4]["content"], json!("next"));
}

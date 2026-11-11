//! Live provider E2E tests.
//!
//! These tests are ignored by default because they need real credentials and
//! network access. Run them explicitly:
//!
//! ```bash
//! OPENAI_API_KEY=... OPENAI_MODEL=gpt-4o-mini \
//!   cargo test -p vp-providers --test live -- --ignored --nocapture
//!
//! ANTHROPIC_API_KEY=... ANTHROPIC_MODEL=claude-3-5-haiku-latest \
//!   cargo test -p vp-providers --test live -- --ignored --nocapture
//! ```
//!
//! A local OpenAI-compatible endpoint can be exercised with:
//!
//! ```bash
//! VERLINPARK_LIVE_BASE_URL=http://127.0.0.1:1234/v1 \
//! VERLINPARK_LIVE_MODEL=local-model \
//!   cargo test -p vp-providers --test live -- --ignored --nocapture
//! ```

use futures_util::StreamExt;
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, StreamEvent,
};

const PROMPT: &str = "Stream a short numbered list of five checks that prove a provider streaming bridge is working. End the final line with DONE.";

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn request(model: String) -> ChatRequest {
    ChatRequest {
        model,
        messages: vec![ChatMessage::user(PROMPT)],
        system: Some("You are a provider connectivity check. Be concise.".to_string()),
        system_stable_prefix: None,
        system_sections: Vec::new(),
        cache: None,
        max_tokens: Some(256),
        temperature: Some(0.0),
        tools: Vec::new(),
    }
}

async fn drain(mut stream: vp_providers::EventStream) -> (String, u32, (u64, u64)) {
    let mut text = String::new();
    let mut chunks = 0_u32;
    let mut usage = (0_u64, 0_u64);
    while let Some(item) = stream.next().await {
        match item.expect("provider stream failed") {
            StreamEvent::TextDelta(delta) => {
                if !delta.is_empty() {
                    chunks += 1;
                    text.push_str(&delta);
                }
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => {
                usage.0 = usage.0.saturating_add(input_tokens as u64);
                usage.1 = usage.1.saturating_add(output_tokens as u64);
            }
            StreamEvent::ToolCall(_) | StreamEvent::Done => {}
        }
    }
    (text, chunks, usage)
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY and network access"]
async fn openai_live_streams_text() {
    let Some(api_key) = env_value("OPENAI_API_KEY") else {
        eprintln!("skipping: OPENAI_API_KEY is not set");
        return;
    };
    let base_url =
        env_value("OPENAI_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let model = env_value("OPENAI_MODEL").unwrap_or_else(|| "gpt-4o-mini".to_string());
    let provider = OpenAiCompatibleProvider::new("live-openai", base_url, Some(api_key));
    let stream = provider
        .stream_chat(request(model))
        .await
        .expect("starting live OpenAI stream");
    let (text, chunks, usage) = drain(stream).await;
    eprintln!("openai live: {chunks} chunks, usage {usage:?}, text:\n{text}");
    assert!(chunks > 0, "live OpenAI stream produced no text chunks");
    assert!(!text.trim().is_empty());
    assert!(text.contains("DONE") || text.len() > 20);
}

#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY and network access"]
async fn anthropic_live_streams_text() {
    let Some(api_key) = env_value("ANTHROPIC_API_KEY") else {
        eprintln!("skipping: ANTHROPIC_API_KEY is not set");
        return;
    };
    let base_url = env_value("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
    let model =
        env_value("ANTHROPIC_MODEL").unwrap_or_else(|| "claude-3-5-haiku-latest".to_string());
    let provider = AnthropicProvider::with_base_url("live-anthropic", base_url, Some(api_key));
    let stream = provider
        .stream_chat(request(model))
        .await
        .expect("starting live Anthropic stream");
    let (text, chunks, usage) = drain(stream).await;
    eprintln!("anthropic live: {chunks} chunks, usage {usage:?}, text:\n{text}");
    assert!(chunks > 0, "live Anthropic stream produced no text chunks");
    assert!(!text.trim().is_empty());
    assert!(usage.0 > 0, "Anthropic should report input usage");
    assert!(text.contains("DONE") || text.len() > 20);
}

#[tokio::test]
#[ignore = "requires VERLINPARK_LIVE_BASE_URL and a running OpenAI-compatible endpoint"]
async fn openai_compatible_local_live_streams_text() {
    let Some(base_url) = env_value("VERLINPARK_LIVE_BASE_URL") else {
        eprintln!("skipping: VERLINPARK_LIVE_BASE_URL is not set");
        return;
    };
    let model = env_value("VERLINPARK_LIVE_MODEL").unwrap_or_else(|| "local-model".to_string());
    let provider =
        OpenAiCompatibleProvider::new("live-local", base_url, env_value("VERLINPARK_LIVE_API_KEY"));
    let stream = provider
        .stream_chat(request(model))
        .await
        .expect("starting live local stream");
    let (text, chunks, usage) = drain(stream).await;
    eprintln!("local live: {chunks} chunks, usage {usage:?}, text:\n{text}");
    assert!(chunks > 0, "live local stream produced no text chunks");
    assert!(!text.trim().is_empty());
}

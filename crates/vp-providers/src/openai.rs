use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};

use crate::{
    ChatRequest, EventStream, Provider, ProviderError, StreamEvent, openai_usage,
    provider_error_from_stream,
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
            client: reqwest::Client::new(),
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
        messages.extend(
            request
                .messages
                .iter()
                .map(|message| json!({"role": message.role, "content": message.content})),
        );

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

        let mut builder = self.client.post(self.endpoint()).json(&body);
        if let Some(api_key) = &self.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let response = builder.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api { status, body });
        }

        let stream = response.bytes_stream().eventsource();
        let mapped = stream.filter_map(|item| async move {
            match item {
                Err(error) => Some(Err(provider_error_from_stream(error))),
                Ok(event) => {
                    let data = event.data;
                    if data.trim() == "[DONE]" {
                        return Some(Ok(StreamEvent::Done));
                    }
                    let value: Value = match serde_json::from_str(&data) {
                        Ok(value) => value,
                        Err(error) => {
                            return Some(Err(ProviderError::Parse(error.to_string())));
                        }
                    };
                    if let Some(text) = value
                        .pointer("/choices/0/delta/content")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        return Some(Ok(StreamEvent::TextDelta(text.to_string())));
                    }
                    if let Some(usage) = openai_usage(&value) {
                        return Some(Ok(usage));
                    }
                    None
                }
            }
        });

        Ok(Box::pin(mapped))
    }
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

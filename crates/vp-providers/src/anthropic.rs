use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{
    ChatRequest, EventStream, Provider, ProviderError, StreamEvent, provider_error_from_stream,
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
            client: reqwest::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/messages", self.base_url)
    }
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
            .map(|message| json!({"role": message.role, "content": message.content}))
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

        let response = self
            .client
            .post(self.endpoint())
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api { status, body });
        }

        let stream = Box::pin(response.bytes_stream().eventsource());
        let mapped = async_stream::stream! {
            let mut input_tokens = 0u32;
            let mut output_tokens = 0u32;
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
                            "content_block_delta" => {
                                if let Some(text) = value
                                    .pointer("/delta/text")
                                    .and_then(Value::as_str)
                                    .filter(|text| !text.is_empty())
                                {
                                    yield Ok(StreamEvent::TextDelta(text.to_string()));
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
}

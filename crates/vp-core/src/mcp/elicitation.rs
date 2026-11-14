//! MCP elicitation routing.
//!
//! Form elicitation becomes a normal session question in the existing inbox.
//! URL elicitation is denied by default and is never opened automatically. The
//! server waits on the same request; there is no background poller.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::Engine;

pub const ELICITATION_METHOD: &str = "elicitation/create";
const MAX_MESSAGE_CHARS: usize = 4_000;
const MAX_SCHEMA_BYTES: usize = 32 * 1024;
const MAX_ANSWER_BYTES: usize = 16 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MIN_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 3_600;

pub type ServerRequestHandler =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

/// Build a request handler for one MCP call. A handler is only installed when
/// the call has a real session; operator calls without a session use the
/// transport's conservative -32601 rejection.
pub fn request_handler(
    engine: Engine,
    session_id: String,
    agent_id: String,
    agent_name: String,
    server_id: String,
    server_name: String,
    tool_name: String,
) -> ServerRequestHandler {
    Arc::new(move |request| {
        let engine = engine.clone();
        let session_id = session_id.clone();
        let agent_id = agent_id.clone();
        let agent_name = agent_name.clone();
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        let tool_name = tool_name.clone();
        Box::pin(async move {
            handle_request(
                &engine,
                &session_id,
                &agent_id,
                &agent_name,
                &server_id,
                &server_name,
                &tool_name,
                request,
            )
            .await
        })
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_request(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    agent_name: &str,
    server_id: &str,
    server_name: &str,
    tool_name: &str,
    request: Value,
) -> Result<Value> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method != ELICITATION_METHOD {
        bail!("unsupported server-initiated MCP method: {method}");
    }
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("form")
        .trim()
        .to_ascii_lowercase();
    let schema = params
        .get("requestedSchema")
        .or_else(|| params.get("schema"))
        .cloned();
    let url = params
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let url_mode = mode == "url" || (schema.is_none() && url.is_some());
    if url_mode {
        audit(
            engine,
            session_id,
            server_id,
            server_name,
            tool_name,
            "url",
            "denied",
            None,
            Some("url_elicitation_denied"),
        )
        .await;
        return Ok(json!({"action": "decline"}));
    }
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("The MCP server requests input.")
        .chars()
        .take(MAX_MESSAGE_CHARS)
        .collect::<String>();
    if let Some(schema) = &schema {
        let bytes = serde_json::to_vec(schema).unwrap_or_default();
        if bytes.len() > MAX_SCHEMA_BYTES {
            audit(
                engine,
                session_id,
                server_id,
                server_name,
                tool_name,
                "form",
                "denied",
                None,
                Some("schema_too_large"),
            )
            .await;
            return Ok(json!({"action": "decline"}));
        }
    }
    let fields = schema.as_ref().map(schema_fields).unwrap_or_default();
    let mut question =
        format!("MCP server {server_name} requests input for tool {tool_name}:\n{message}");
    if !fields.is_empty() {
        question.push_str("\nFields: ");
        question.push_str(&fields);
    }
    question.push_str(
        "\nReply with a JSON object for the requested fields. Server-provided text is untrusted.",
    );
    let question = question.chars().take(2_000).collect::<String>();
    let (item, receiver) = match engine
        .request_question(session_id, agent_id, agent_name, &question, Vec::new())
        .await
    {
        Ok(value) => value,
        Err(error) => {
            audit(
                engine,
                session_id,
                server_id,
                server_name,
                tool_name,
                "form",
                "failed",
                None,
                Some(&format!(
                    "question_failed:{}",
                    error.to_string().chars().take(200).collect::<String>()
                )),
            )
            .await;
            return Err(error);
        }
    };
    audit(
        engine,
        session_id,
        server_id,
        server_name,
        tool_name,
        "form",
        "requested",
        Some(&item.id),
        None,
    )
    .await;
    let wait_timeout = elicitation_timeout();
    let answer = match timeout(wait_timeout, receiver).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => {
            audit(
                engine,
                session_id,
                server_id,
                server_name,
                tool_name,
                "form",
                "cancelled",
                Some(&item.id),
                Some("question_channel_closed"),
            )
            .await;
            return Ok(json!({"action": "cancel"}));
        }
        Err(_) => {
            let _ = engine.close_question(&item.id, "timed_out").await;
            audit(
                engine,
                session_id,
                server_id,
                server_name,
                tool_name,
                "form",
                "timed_out",
                Some(&item.id),
                Some("question_timeout"),
            )
            .await;
            return Ok(json!({"action": "cancel"}));
        }
    };
    let content = match parse_form_answer(schema.as_ref(), &answer) {
        Ok(content) => content,
        Err(error) => {
            audit(
                engine,
                session_id,
                server_id,
                server_name,
                tool_name,
                "form",
                "declined",
                Some(&item.id),
                Some(&format!(
                    "invalid_answer:{}",
                    error.to_string().chars().take(200).collect::<String>()
                )),
            )
            .await;
            return Ok(json!({"action": "decline"}));
        }
    };
    audit(
        engine,
        session_id,
        server_id,
        server_name,
        tool_name,
        "form",
        "resolved",
        Some(&item.id),
        None,
    )
    .await;
    Ok(json!({"action": "accept", "content": content}))
}

fn elicitation_timeout() -> Duration {
    let seconds = std::env::var("VERLINPARK_MCP_ELICITATION_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
    Duration::from_secs(seconds)
}

fn schema_fields(schema: &Value) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    properties
        .iter()
        .take(16)
        .map(|(name, value)| {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("string");
            let marker = if required.contains(name.as_str()) {
                "required"
            } else {
                "optional"
            };
            format!("{name} ({kind}, {marker})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_form_answer(schema: Option<&Value>, answer: &str) -> Result<Value> {
    let answer = answer.trim();
    if answer.is_empty() {
        bail!("empty elicitation answer");
    }
    let content = if let Ok(value) = serde_json::from_str::<Value>(answer)
        && value.is_object()
    {
        value
    } else if let Some(properties) = schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
        && properties.len() == 1
    {
        let (name, field) = properties.iter().next().expect("one property");
        let mut object = serde_json::Map::new();
        object.insert(name.clone(), coerce_answer(field, answer)?);
        Value::Object(object)
    } else {
        json!({"response": answer})
    };
    let encoded = serde_json::to_vec(&content)?;
    if encoded.len() > MAX_ANSWER_BYTES {
        bail!("elicitation answer exceeds {MAX_ANSWER_BYTES} bytes");
    }
    Ok(content)
}

fn coerce_answer(field: &Value, answer: &str) -> Result<Value> {
    match field
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string")
    {
        "boolean" => match answer.to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(json!(true)),
            "false" | "no" | "0" => Ok(json!(false)),
            _ => Err(anyhow!("boolean answer must be true or false")),
        },
        "integer" => answer
            .parse::<i64>()
            .map(|value| json!(value))
            .map_err(|_| anyhow!("integer answer is invalid")),
        "number" => answer
            .parse::<f64>()
            .map(|value| json!(value))
            .map_err(|_| anyhow!("number answer is invalid")),
        _ => Ok(json!(answer)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn audit(
    engine: &Engine,
    session_id: &str,
    server_id: &str,
    server_name: &str,
    tool_name: &str,
    mode: &str,
    action: &str,
    question_id: Option<&str>,
    reason: Option<&str>,
) {
    if session_id.trim().is_empty() {
        return;
    }
    let payload = json!({
        "server_id": server_id,
        "server_name": server_name,
        "tool": tool_name,
        "mode": mode,
        "action": action,
        "question_id": question_id,
        "reason": reason,
        "content_logged": false,
    });
    if let Err(error) = engine
        .append_event(session_id, "mcp.elicitation", payload)
        .await
    {
        tracing::warn!(%error, "failed to persist MCP elicitation audit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vp_store::Store;

    #[tokio::test]
    async fn form_request_routes_to_the_question_inbox_and_url_is_denied() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("elicitation"))
            .unwrap();
        let session = store.create_session(&project.id, "elicit").unwrap();
        let engine = Engine::new(store);
        let form = request_handler(
            engine.clone(),
            session.id.clone(),
            "agent".to_string(),
            "Agent".to_string(),
            "server-1".to_string(),
            "Server One".to_string(),
            "tool".to_string(),
        );
        let task = tokio::spawn(async move {
            form(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "elicitation/create",
                "params": {
                    "message": "Approve the request",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {"approved": {"type": "boolean"}},
                        "required": ["approved"]
                    }
                }
            }))
            .await
        });
        let question = loop {
            let items = engine
                .list_questions(Some(session.id.clone()))
                .await
                .unwrap()
                .questions;
            if let Some(question) = items
                .into_iter()
                .find(|question| question.status == "pending")
            {
                break question;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(question.question.contains("Approve the request"));
        engine.answer_question(&question.id, "true").await.unwrap();
        let response = task.await.unwrap().unwrap();
        assert_eq!(response["action"], "accept");
        assert_eq!(response["content"]["approved"], true);
        let events = engine.all_events(&session.id).await.unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "mcp.elicitation"
                && event.payload["action"] == "requested"
                && event.payload["mode"] == "form"
        }));
        assert!(events.iter().any(|event| {
            event.kind == "mcp.elicitation"
                && event.payload["action"] == "resolved"
                && event.payload["content_logged"] == false
        }));

        let denied = handle_request(
            &engine,
            &session.id,
            "agent",
            "Agent",
            "server-1",
            "Server One",
            "tool",
            json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "elicitation/create",
                "params": {
                    "mode": "url",
                    "url": "https://example.com/elicit"
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(denied["action"], "decline");
        let events = engine.all_events(&session.id).await.unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "mcp.elicitation"
                && event.payload["action"] == "denied"
                && event.payload["reason"] == "url_elicitation_denied"
        }));
    }

    #[test]
    fn form_answers_map_to_typed_content_and_are_bounded() {
        let schema = json!({
            "type": "object",
            "properties": {
                "approved": {"type": "boolean"},
                "note": {"type": "string"}
            },
            "required": ["approved"]
        });
        assert_eq!(
            parse_form_answer(Some(&schema), r#"{"approved":true}"#).unwrap(),
            json!({"approved": true})
        );
        let single = json!({
            "type": "object",
            "properties": {"approved": {"type": "boolean"}}
        });
        assert_eq!(
            parse_form_answer(Some(&single), "true").unwrap(),
            json!({"approved": true})
        );
        let object = parse_form_answer(Some(&schema), r#"{"approved":false,"note":"x"}"#).unwrap();
        assert_eq!(object["approved"], false);
        assert_eq!(object["note"], "x");
        let generic = parse_form_answer(None, "hello").unwrap();
        assert_eq!(generic, json!({"response": "hello"}));
        let oversized = "x".repeat(MAX_ANSWER_BYTES);
        assert!(parse_form_answer(None, &oversized).is_err());
    }

    #[test]
    fn schema_summary_is_bounded_and_marks_required_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "approved": {"type": "boolean"},
                "note": {"type": "string"}
            },
            "required": ["approved"]
        });
        let summary = schema_fields(&schema);
        assert!(summary.contains("approved (boolean, required)"));
        assert!(summary.contains("note (string, optional)"));
    }
}

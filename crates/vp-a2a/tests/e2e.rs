//! End-to-end loopback tests: real TCP listener, real reqwest client, real SSE.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use vp_a2a::{
    A2aClient, A2aErrorCode, A2aHandler, A2aPolicy, A2aRequestContext, A2aServer, A2aStartHandle,
    A2aStreamEvent, A2aTaskUpdate, Artifact, Message, Part, TaskState,
};

const TOKEN: &str = "e2e-token-0123456789";

struct StructHandler;

impl A2aHandler for StructHandler {
    async fn handle(&self, _request: A2aRequestContext) -> vp_a2a::A2aResult<A2aTaskUpdate> {
        Ok(A2aTaskUpdate::completed())
    }
}

fn policy() -> A2aPolicy {
    A2aPolicy::new("e2e agent", "end-to-end test agent", "1.0.0")
        .with_bearer_token(TOKEN)
        .expect("token")
        .enable()
}

fn message_text(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn handler(calls: Arc<AtomicUsize>) -> impl A2aHandler {
    move |context: A2aRequestContext| {
        let calls = Arc::clone(&calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let text = message_text(&context.message);
            match text.as_str() {
                "question" => Ok(A2aTaskUpdate::new(TaskState::InputRequired)),
                "auth" => Ok(A2aTaskUpdate::new(TaskState::AuthRequired)),
                "cancel" => {
                    tokio::time::timeout(Duration::from_secs(5), context.cancellation.cancelled())
                        .await
                        .ok();
                    Ok(A2aTaskUpdate::new(TaskState::Completed))
                }
                "answer" => Ok(A2aTaskUpdate::new(TaskState::Completed)
                    .with_message(Message::agent_text("agent-answer", "answered"))),
                "artifact" => Ok(A2aTaskUpdate::new(TaskState::Completed)
                    .with_message(Message::agent_text("agent-artifact", "artifact ready"))
                    .with_artifact(Artifact::new(
                        "artifact-1",
                        vec![Part::text("artifact body")],
                    ))),
                _ => Ok(A2aTaskUpdate::new(TaskState::Completed)
                    .with_message(Message::agent_text("agent-default", "done"))),
            }
        }
    }
}

async fn start(calls: Arc<AtomicUsize>) -> A2aStartHandle {
    A2aServer::start(policy(), handler(calls))
        .await
        .expect("start facade")
}

fn auth(token: &str) -> String {
    format!("Bearer {token}")
}

fn rpc_body(method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    })
}

fn parse_sse(text: &str) -> Vec<(Option<u64>, Value)> {
    let mut frames = Vec::new();
    let mut id = None;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("id: ") {
            id = Some(value.parse::<u64>().expect("numeric SSE id"));
        } else if let Some(value) = line.strip_prefix("data: ") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value);
        } else if line.is_empty() && !data.is_empty() {
            let value = serde_json::from_str(&data).expect("SSE JSON");
            frames.push((id.take(), value));
            data.clear();
        }
    }
    frames
}

async fn collect_client_stream(mut stream: vp_a2a::A2aEventStream) -> Vec<vp_a2a::A2aEventFrame> {
    let mut frames = Vec::new();
    while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream did not stall")
    {
        frames.push(frame.expect("stream frame"));
    }
    frames
}

#[tokio::test]
async fn card_send_get_duplicate_and_error_paths() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = start(Arc::clone(&calls)).await;
    let base = handle.url().to_string();
    let http = reqwest::Client::new();

    let unauthorized = http
        .get(format!("{base}/.well-known/agent-card.json"))
        .send()
        .await
        .expect("unauthorized GET");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert!(unauthorized.headers().contains_key("www-authenticate"));
    let unauthorized_body = unauthorized.text().await.expect("unauthorized body");
    assert!(!unauthorized_body.contains(TOKEN));

    let wrong = http
        .get(format!("{base}/.well-known/agent-card.json"))
        .header("authorization", auth("wrong-token"))
        .send()
        .await
        .expect("wrong token GET");
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let card_response = http
        .get(format!("{base}/.well-known/agent-card.json"))
        .header("authorization", auth(TOKEN))
        .send()
        .await
        .expect("card GET");
    assert_eq!(card_response.status(), StatusCode::OK);
    let card_text = card_response.text().await.expect("card body");
    assert!(!card_text.contains(TOKEN));
    let card: Value = serde_json::from_str(&card_text).expect("card JSON");
    assert_eq!(card["url"].as_str(), Some(base.as_str()));
    assert_eq!(card["protocolVersion"], "1.0");
    assert_eq!(card["capabilities"]["streaming"], true);
    assert!(card["securitySchemes"]["bearerAuth"].is_object());

    let send = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body(
            "message/send",
            json!({"message": Message::user_text("m-1", "hello")}),
        ))
        .send()
        .await
        .expect("send");
    assert_eq!(send.status(), StatusCode::OK);
    let send_json: Value = send.json().await.expect("send JSON");
    let task_id = send_json["result"]["id"]
        .as_str()
        .expect("task id")
        .to_string();
    assert_eq!(task_id, "task-0000000000000001");
    assert_eq!(send_json["result"]["status"]["state"], "completed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let events = http
        .get(format!("{base}/a2a/tasks/{task_id}/events"))
        .header("authorization", auth(TOKEN))
        .send()
        .await
        .expect("task events GET");
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(
        events
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
        "text/event-stream"
    );
    let event_text = events.text().await.expect("event body");
    let event_frames = parse_sse(&event_text);
    let event_ids: Vec<u64> = event_frames
        .iter()
        .map(|(id, _)| id.expect("SSE event id"))
        .collect();
    assert_eq!(event_ids, vec![1, 2, 3]);
    assert_eq!(event_frames[0].1["status"]["state"], "submitted");
    assert_eq!(event_frames[1].1["status"]["state"], "working");
    assert_eq!(event_frames[2].1["status"]["state"], "completed");
    assert_eq!(event_frames[2].1["final"], true);

    let get = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body("tasks/get", json!({"id": task_id})))
        .send()
        .await
        .expect("get");
    let get_json: Value = get.json().await.expect("get JSON");
    assert_eq!(get_json["result"]["id"].as_str(), Some(task_id.as_str()));
    assert_eq!(
        get_json["result"]["history"]
            .as_array()
            .expect("history")
            .len(),
        2
    );

    // A duplicate messageId returns the existing task and never calls the
    // handler again.
    let duplicate = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body(
            "message/send",
            json!({"message": Message::user_text("m-1", "hello")}),
        ))
        .send()
        .await
        .expect("duplicate");
    let duplicate_json: Value = duplicate.json().await.expect("duplicate JSON");
    assert_eq!(
        duplicate_json["result"]["id"].as_str(),
        Some(task_id.as_str())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let conflicting = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body(
            "message/send",
            json!({"message": Message::user_text("m-1", "different body")}),
        ))
        .send()
        .await
        .expect("conflict");
    let conflict_json: Value = conflicting.json().await.expect("conflict JSON");
    assert_eq!(conflict_json["error"]["code"], -32014);
    assert_eq!(
        conflict_json["error"]["data"]["a2aCode"],
        "IDEMPOTENCY_CONFLICT"
    );

    let cancel_terminal = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body("tasks/cancel", json!({"id": task_id})))
        .send()
        .await
        .expect("cancel terminal");
    let cancel_json: Value = cancel_terminal.json().await.expect("cancel JSON");
    assert_eq!(cancel_json["error"]["code"], -32002);

    let missing = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body("tasks/get", json!({"id": "task-missing"})))
        .send()
        .await
        .expect("missing task");
    let missing_json: Value = missing.json().await.expect("missing JSON");
    assert_eq!(missing_json["error"]["code"], -32001);
    assert_eq!(missing_json["error"]["data"]["a2aCode"], "TASK_NOT_FOUND");

    let unknown_method = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body("agent/card", json!({})))
        .send()
        .await
        .expect("unknown method");
    let unknown_json: Value = unknown_method.json().await.expect("unknown JSON");
    assert_eq!(unknown_json["error"]["code"], -32601);

    let too_many_parts = (0..33).map(|i| Part::text(format!("p{i}"))).collect();
    let too_many_parts = Message::new(vp_a2a::MessageRole::User, "m-parts", too_many_parts);
    let bounds = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .json(&rpc_body(
            "message/send",
            json!({"message": too_many_parts}),
        ))
        .send()
        .await
        .expect("bounds");
    let bounds_json: Value = bounds.json().await.expect("bounds JSON");
    assert_eq!(bounds_json["error"]["code"], -32602);
    assert_eq!(bounds_json["error"]["data"]["a2aCode"], "BOUNDS_EXCEEDED");

    let oversized = "x".repeat(vp_a2a::MAX_BODY_BYTES + 16);
    let body_too_large = http
        .post(format!("{base}/a2a"))
        .header("authorization", auth(TOKEN))
        .header("content-type", "application/json")
        .body(oversized)
        .send()
        .await
        .expect("oversized body");
    assert_eq!(body_too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);

    handle.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn lifecycle_input_required_and_artifact_stream() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = start(Arc::clone(&calls)).await;
    let token = handle.take_token().expect("token");
    let client = A2aClient::new(handle.url(), token).expect("client");

    let card = client.card().await.expect("card");
    assert_eq!(card.url, handle.url());

    let first = client
        .send(Message::user_text("m-1", "question"))
        .await
        .expect("question");
    assert_eq!(first.status.state, TaskState::InputRequired);
    let task_id = first.id.clone();
    let context_id = first.context_id.clone();

    let answer = Message::user_text("m-2", "answer")
        .with_task(task_id.clone())
        .with_context(context_id);
    let second = client.send(answer).await.expect("answer");
    assert_eq!(second.status.state, TaskState::Completed);
    assert_eq!(second.id, task_id);
    assert_eq!(second.history.len(), 3);

    let auth = client
        .send(Message::user_text("m-auth", "auth"))
        .await
        .expect("auth required");
    assert_eq!(auth.status.state, TaskState::AuthRequired);
    let auth_answer = Message::user_text("m-auth-answer", "answer")
        .with_task(auth.id.clone())
        .with_context(auth.context_id.clone());
    let auth_done = client.send(auth_answer).await.expect("auth follow-up");
    assert_eq!(auth_done.status.state, TaskState::Completed);

    let accepted = client
        .send_non_blocking(Message::user_text("m-nonblocking", "hello"))
        .await
        .expect("non-blocking send");
    assert_eq!(accepted.status.state, TaskState::Working);
    let completion_frames = collect_client_stream(
        client
            .subscribe(&accepted.id, None)
            .await
            .expect("subscribe non-blocking task"),
    )
    .await;
    assert!(matches!(
        &completion_frames.last().expect("non-blocking final").event,
        A2aStreamEvent::Status(event)
            if event.status.state == TaskState::Completed && event.is_final
    ));

    let streamed = client
        .stream(Message::user_text("m-3", "artifact"))
        .await
        .expect("stream");
    let frames = collect_client_stream(streamed).await;
    assert!(!frames.is_empty());
    assert!(frames.windows(2).all(|pair| {
        match (pair[0].id, pair[1].id) {
            (Some(left), Some(right)) => left < right,
            (None, None) => true,
            _ => false,
        }
    }));
    assert!(matches!(
        &frames[0].event,
        A2aStreamEvent::Status(event) if event.status.state == TaskState::Submitted
    ));
    assert!(matches!(
        &frames[1].event,
        A2aStreamEvent::Status(event) if event.status.state == TaskState::Working
    ));
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame.event, A2aStreamEvent::Artifact(_)))
    );
    assert!(matches!(
        &frames.last().expect("last frame").event,
        A2aStreamEvent::Status(event)
            if event.status.state == TaskState::Completed && event.is_final
    ));

    let streaming_task = frames
        .last()
        .map(|frame| match &frame.event {
            A2aStreamEvent::Status(event) => event.task_id.clone(),
            A2aStreamEvent::Artifact(event) => event.task_id.clone(),
        })
        .expect("task id");
    let replay = client
        .subscribe(&streaming_task, None)
        .await
        .expect("subscribe");
    let replay_frames = collect_client_stream(replay).await;
    assert!(replay_frames.len() >= 4);
    assert!(matches!(
        &replay_frames.last().expect("replay final").event,
        A2aStreamEvent::Status(event) if event.is_final
    ));

    let resubscribed = client
        .resubscribe(&streaming_task, Some(replay_frames[0].id.unwrap_or(0)))
        .await
        .expect("resubscribe");
    let resubscribed_frames = collect_client_stream(resubscribed).await;
    assert!(
        resubscribed_frames
            .iter()
            .all(|frame| frame.id.unwrap_or(0) > replay_frames[0].id.unwrap_or(0))
    );

    handle.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn streaming_cancel_is_terminal_and_sse_is_bounded() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handle = start(calls).await;
    let token = handle.take_token().expect("token");
    let client = A2aClient::new(handle.url(), token).expect("client");

    let mut stream = client
        .stream(Message::user_text("m-cancel", "cancel"))
        .await
        .expect("cancel stream");
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("first frame")
        .expect("stream open")
        .expect("first frame result");
    let task_id = match &first.event {
        A2aStreamEvent::Status(event) => event.task_id.clone(),
        A2aStreamEvent::Artifact(event) => event.task_id.clone(),
    };
    let canceled = client.cancel(&task_id).await.expect("cancel task");
    assert_eq!(canceled.status.state, TaskState::Canceled);

    let mut saw_canceled_final = false;
    for _ in 0..16 {
        let Some(Ok(frame)) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("stream progress")
        else {
            break;
        };
        if let A2aStreamEvent::Status(event) = frame.event
            && event.status.state == TaskState::Canceled
        {
            assert!(event.is_final);
            saw_canceled_final = true;
            break;
        }
    }
    assert!(saw_canceled_final);

    let fetched = client.get_task(&task_id).await.expect("task");
    assert_eq!(fetched.status.state, TaskState::Canceled);
    let repeated = client.cancel(&task_id).await.expect("idempotent cancel");
    assert_eq!(repeated.status.state, TaskState::Canceled);

    handle.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn disabled_and_public_policies_never_open_a_listener() {
    let disabled = A2aPolicy::default();
    let error = A2aServer::start(disabled, StructHandler)
        .await
        .expect_err("disabled policy");
    assert_eq!(error.code(), A2aErrorCode::PolicyDisabled);

    assert!("0.0.0.0:0".parse::<vp_a2a::A2aListenAddr>().is_err());
    assert!(A2aClient::new("http://192.168.1.2:8080", TOKEN).is_err());
}

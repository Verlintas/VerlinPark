//! End-to-end loopback tests for the public A2A facade.

use std::time::Duration;

use futures_util::StreamExt;
use vp_a2a::{
    A2aClient, A2aPolicy, A2aRequestContext, A2aServer, A2aStreamEvent, A2aTaskUpdate, Message,
    TaskState,
};

fn policy() -> A2aPolicy {
    A2aPolicy::new("test", "loopback test", "1.0.0")
        .with_bearer_token("0123456789abcdef0123456789abcdef")
        .unwrap()
        .with_listen("127.0.0.1:0")
        .unwrap()
        .enable()
}

async fn start_completed() -> (vp_a2a::A2aStartHandle, String) {
    let server = A2aServer::start(policy(), |request: A2aRequestContext| async move {
        Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Completed).with_status(
            vp_a2a::TaskStatus::new(TaskState::Completed).with_message(Message::agent_text(
                format!("done-{}", request.task_id),
                "done",
            )),
        ))
    })
    .await
    .expect("start loopback server");
    let token = server.take_token().expect("one-time token");
    (server, token)
}

#[tokio::test]
async fn card_send_get_and_stream_are_bounded_and_ordered() {
    let (server, token) = start_completed().await;
    let client = A2aClient::new(server.url(), token).unwrap();
    let card = client.card().await.unwrap();
    assert_eq!(card.name, "test");
    assert!(card.url.starts_with("http://127.0.0.1:"));
    assert!(card.capabilities.streaming);

    let task = client
        .send(Message::user_text("message-1", "hello"))
        .await
        .unwrap();
    assert_eq!(task.status.state, TaskState::Completed);
    assert!(!task.history.is_empty());
    let fetched = client.get_task(&task.id).await.unwrap();
    assert_eq!(fetched.id, task.id);
    assert_eq!(fetched.status.state, TaskState::Completed);

    let mut stream = client
        .stream(Message::user_text("message-2", "stream"))
        .await
        .unwrap();
    let mut kinds = Vec::new();
    let mut final_seen = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Some(item)) = tokio::time::timeout_at(deadline, stream.next()).await {
        let frame = item.unwrap();
        let event: A2aStreamEvent = frame.event;
        kinds.push(event.kind());
        if let A2aStreamEvent::Status(status) = event
            && status.is_final
        {
            final_seen = true;
            break;
        }
    }
    assert!(final_seen, "stream must emit a final status: {kinds:?}");
    assert!(!kinds.is_empty());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn wrong_token_and_public_urls_fail_closed() {
    let (server, token) = start_completed().await;
    let wrong = A2aClient::new(server.url(), "wrong-token-0123456789").unwrap();
    let error = wrong.card().await.unwrap_err();
    assert!(matches!(error, vp_a2a::A2aError::Unauthorized));

    let public = A2aPolicy::new("test", "loopback test", "1.0.0")
        .with_bearer_token("0123456789abcdef0123456789abcdef")
        .unwrap();
    assert!(public.with_listen("0.0.0.0:0").is_err());
    assert!(!token.is_empty());

    let mut disabled = A2aPolicy::default();
    disabled = disabled
        .with_bearer_token("0123456789abcdef0123456789abcdef")
        .unwrap();
    assert!(
        A2aServer::start(disabled, |_request: A2aRequestContext| async {
            Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Rejected))
        })
        .await
        .is_err()
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_task_cancel_is_rejected_without_mutation() {
    let (server, token) = start_completed().await;
    let client = A2aClient::new(server.url(), token).unwrap();
    let task = client
        .send(Message::user_text("message-3", "cancel me"))
        .await
        .unwrap();
    let error = client.cancel(&task.id).await.unwrap_err();
    assert!(matches!(
        error,
        vp_a2a::A2aError::TaskTerminal { .. }
            | vp_a2a::A2aError::TaskNotCancelable { .. }
            | vp_a2a::A2aError::TaskNotFound { .. }
    ));
    let fetched = client.get_task(&task.id).await.unwrap();
    assert_eq!(fetched.status.state, TaskState::Completed);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn idempotent_duplicate_message_does_not_create_two_tasks() {
    let (server, token) = start_completed().await;
    let client = A2aClient::new(server.url(), token).unwrap();
    let message = Message::user_text("message-duplicate", "once");
    let first = client.send(message.clone()).await.unwrap();
    let second = client.send(message).await.unwrap();
    assert_eq!(first.id, second.id);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn stream_is_send_and_bounded() {
    let (server, token) = start_completed().await;
    let client = A2aClient::new(server.url(), token).unwrap();
    let stream = client
        .stream(Message::user_text("message-4", "bounded"))
        .await
        .unwrap();
    drop(stream);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn client_rejects_non_loopback_base_url() {
    let error = A2aClient::new("http://example.com:1234", "token").unwrap_err();
    assert!(matches!(error, vp_a2a::A2aError::LoopbackRequired { .. }));
}

#[tokio::test]
async fn server_handler_receives_context_without_tool_authority() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<(String, String)>));
    let seen_clone = seen.clone();
    let server = A2aServer::start(policy(), move |request: A2aRequestContext| {
        let seen = seen_clone.clone();
        async move {
            if let Ok(mut slot) = seen.lock() {
                *slot = Some((request.task_id.clone(), request.context_id.clone()));
            }
            Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Completed))
        }
    })
    .await
    .unwrap();
    let token = server.take_token().unwrap();
    let client = A2aClient::new(server.url(), token).unwrap();
    let task = client
        .send(Message::user_text("message-5", "context"))
        .await
        .unwrap();
    let (task_id, context_id) = seen.lock().unwrap().clone().unwrap();
    assert_eq!(task_id, task.id);
    assert_eq!(context_id, task.context_id);
    server.shutdown().await.unwrap();
}

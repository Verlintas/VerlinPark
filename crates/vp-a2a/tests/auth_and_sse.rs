use std::time::Duration;

use futures_util::StreamExt;
use vp_a2a::{
    A2aClient, A2aPolicy, A2aRequestContext, A2aServer, A2aStreamEvent, A2aTaskUpdate, Message,
    TaskState,
};

async fn start() -> (vp_a2a::A2aStartHandle, String) {
    let server = A2aServer::start(
        A2aPolicy::new("auth", "auth test", "1.0.0")
            .with_bearer_token("0123456789abcdef0123456789abcdef")
            .unwrap()
            .with_listen("127.0.0.1:0")
            .unwrap()
            .enable(),
        |_request: A2aRequestContext| async {
            Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Completed))
        },
    )
    .await
    .unwrap();
    let token = server.take_token().unwrap();
    (server, token)
}

#[tokio::test]
async fn unauthorized_send_and_card_are_rejected_without_task_creation() {
    let (server, token) = start().await;
    let wrong = A2aClient::new(server.url(), "not-the-token").unwrap();
    assert!(wrong.card().await.is_err());
    assert!(wrong.send(Message::user_text("m1", "hello")).await.is_err());
    let good = A2aClient::new(server.url(), token).unwrap();
    let task = good.send(Message::user_text("m2", "hello")).await.unwrap();
    assert_eq!(task.status.state, TaskState::Completed);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_message_is_rejected_before_handler_execution() {
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_clone = seen.clone();
    let server = A2aServer::start(
        A2aPolicy::new("bounds", "bounds test", "1.0.0")
            .with_bearer_token("0123456789abcdef0123456789abcdef")
            .unwrap()
            .with_listen("127.0.0.1:0")
            .unwrap()
            .enable(),
        move |_request: A2aRequestContext| {
            let seen = seen_clone.clone();
            async move {
                seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Completed))
            }
        },
    )
    .await
    .unwrap();
    let token = server.take_token().unwrap();
    let client = A2aClient::new(server.url(), token).unwrap();
    let huge = "x".repeat(128 * 1024);
    let result = client.send(Message::user_text("m3", huge)).await;
    assert!(result.is_err());
    assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn stream_is_ordered_and_ends_with_a_final_status() {
    let (server, token) = start().await;
    let client = A2aClient::new(server.url(), token).unwrap();
    let mut stream = client
        .stream(Message::user_text("m4", "stream"))
        .await
        .unwrap();
    let mut event_ids = Vec::new();
    let mut final_seen = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Some(item)) = tokio::time::timeout_at(deadline, stream.next()).await {
        let frame = item.unwrap();
        event_ids.push(frame.id);
        if let A2aStreamEvent::Status(status) = frame.event
            && status.is_final
        {
            final_seen = true;
            break;
        }
    }
    assert!(final_seen);
    assert!(!event_ids.is_empty());
    let mut sorted = event_ids.clone();
    sorted.sort();
    assert_eq!(event_ids, sorted, "SSE ids must be monotonic");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancel_of_completed_task_never_mutates_it() {
    let (server, token) = start().await;
    let client = A2aClient::new(server.url(), token).unwrap();
    let task = client.send(Message::user_text("m5", "done")).await.unwrap();
    assert!(client.cancel(&task.id).await.is_err());
    let fetched = client.get_task(&task.id).await.unwrap();
    assert_eq!(fetched.status.state, TaskState::Completed);
    server.shutdown().await.unwrap();
}

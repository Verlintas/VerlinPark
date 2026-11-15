use tempfile::TempDir;
use vp_a2a::{A2aClient, Message, TaskState};
use vp_core::Engine;
use vp_protocol::a2a::A2aPolicy;
use vp_store::Store;

fn engine() -> (TempDir, Engine) {
    let data = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    (data, Engine::new(store))
}

#[tokio::test]
async fn a2a_facade_is_disabled_by_default_and_requires_approval() {
    let (_data, engine) = engine();
    let status = engine.a2a_status();
    assert!(!status.enabled);
    assert!(!status.running);
    assert!(status.url.is_none());
    assert!(!status.token_present);

    let policy = A2aPolicy {
        enabled: true,
        ..Default::default()
    };
    let unapproved = engine.a2a_start(false, Some(policy.clone())).await;
    assert!(unapproved.is_err(), "unapproved start must fail closed");
    let public = A2aPolicy {
        enabled: true,
        bind_host: "0.0.0.0".into(),
        ..Default::default()
    };
    assert!(engine.a2a_start(true, Some(public)).await.is_err());
}

#[tokio::test]
async fn a2a_loopback_card_and_card_only_task_rejection_round_trip() {
    let (_data, engine) = engine();
    let policy = A2aPolicy {
        enabled: true,
        allow_inbound: false,
        port: 0,
        max_tasks: 8,
        task_ttl_secs: 60,
        ..Default::default()
    };
    let started = engine.a2a_start(true, Some(policy)).await.unwrap();
    assert!(started.status.running);
    assert!(started.status.token_present);
    assert!(started.status.token_issued);
    assert!(!started.status.inbound_enabled);
    let url = started.status.url.clone().expect("loopback url");
    assert!(url.starts_with("http://127.0.0.1:"));
    let token = started.token.expect("one-time token");
    assert_eq!(token.len(), 64);

    let client = A2aClient::new(url, token).unwrap();
    let card = client.card().await.unwrap();
    assert!(card.url.starts_with("http://127.0.0.1:"));
    let task = client
        .send(Message::user_text("msg-1", "please inspect"))
        .await
        .unwrap();
    assert_eq!(task.status.state, TaskState::Rejected);
    assert_eq!(engine.a2a_status().task_count, 1);
    let tasks = engine.a2a_tasks(10, Some("rejected"));
    assert_eq!(tasks.tasks.len(), 1);
    assert_eq!(tasks.tasks[0].id, task.id);

    let stopped = engine.a2a_stop(Some("test complete")).await.unwrap();
    assert!(!stopped.running);
    assert!(stopped.url.is_none());
}

#[tokio::test]
async fn a2a_policy_is_stopped_mutable_and_card_is_read_only() {
    let (_data, engine) = engine();
    let policy = A2aPolicy {
        max_tasks: 12,
        ..A2aPolicy::default()
    };
    let status = engine.a2a_set_policy(policy.clone()).unwrap();
    assert!(!status.running);
    assert_eq!(status.max_tasks, 12);
    assert!(engine.a2a_card().is_none());

    let started = engine
        .a2a_start(
            true,
            Some(A2aPolicy {
                enabled: true,
                port: 0,
                max_tasks: 12,
                task_ttl_secs: 60,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert!(engine.a2a_card().is_some());
    let card = engine.a2a_card().unwrap();
    assert!(
        card["url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("http://127.0.0.1:")
    );
    let changed = A2aPolicy {
        max_tasks: 20,
        ..policy.clone()
    };
    assert!(
        engine.a2a_set_policy(changed).is_err(),
        "policy mutation while running must fail closed"
    );
    engine.a2a_stop(None).await.unwrap();
    assert!(engine.a2a_card().is_none());
    assert!(engine.a2a_set_policy(policy).is_ok());
    assert!(started.token.is_some());
}

#[tokio::test]
async fn a2a_task_cap_is_enforced_for_status_and_runtime_store() {
    let (_data, engine) = engine();
    let started = engine
        .a2a_start(
            true,
            Some(A2aPolicy {
                enabled: true,
                allow_inbound: false,
                port: 0,
                max_tasks: 1,
                task_ttl_secs: 60,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let client = A2aClient::new(
        started.status.url.clone().expect("loopback url"),
        started.token.expect("one-time token"),
    )
    .unwrap();
    let first = client
        .send(Message::user_text("cap-message-0", "request-0"))
        .await
        .unwrap();
    assert_eq!(first.status.state, TaskState::Rejected);
    let second = client
        .send(Message::user_text("cap-message-1", "request-1"))
        .await;
    assert!(
        second.is_err(),
        "the runtime task store must enforce max_tasks"
    );
    let tasks = engine.a2a_tasks(200, None);
    assert_eq!(tasks.tasks.len(), 1);
    assert_eq!(engine.a2a_status().task_count, 1);
    assert_eq!(engine.a2a_status().max_tasks, 1);
    engine.a2a_stop(None).await.unwrap();
}

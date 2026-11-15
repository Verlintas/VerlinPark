use tempfile::TempDir;
use vp_core::Engine;
use vp_protocol::a2a::A2aPolicy;
use vp_store::Store;

fn engine() -> (TempDir, Engine) {
    let data = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    (data, Engine::new(store))
}

#[tokio::test]
async fn a2a_admin_lifecycle_is_explicit_and_token_redacted() {
    let (_data, engine) = engine();
    assert!(!engine.a2a_status().enabled);
    assert!(engine.a2a_card().is_none());
    assert!(engine.a2a_tasks(10, None).tasks.is_empty());

    let disabled = A2aPolicy::default();
    assert!(engine.a2a_start(true, Some(disabled)).await.is_err());

    let policy = A2aPolicy {
        enabled: true,
        port: 0,
        max_tasks: 8,
        task_ttl_secs: 60,
        ..Default::default()
    };
    let status = engine.a2a_set_policy(policy.clone()).unwrap();
    assert!(!status.running);
    assert_eq!(status.max_tasks, 8);

    let started = engine.a2a_start(true, Some(policy.clone())).await.unwrap();
    assert!(
        started
            .token
            .as_deref()
            .is_some_and(|token| token.len() == 64)
    );
    let status_json = serde_json::to_value(&started.status).unwrap();
    assert!(status_json.get("token").is_none());
    assert_eq!(status_json["token_present"], true);
    assert_eq!(status_json["token_issued"], true);
    assert!(engine.a2a_start(true, Some(policy)).await.is_err());

    let stopped = engine.a2a_stop(Some("admin test")).await.unwrap();
    assert!(!stopped.running);
    assert!(stopped.url.is_none());
    assert!(engine.a2a_card().is_none());
    let stopped_again = engine.a2a_stop(None).await.unwrap();
    assert!(!stopped_again.running);
}

#[tokio::test]
async fn a2a_policy_rejects_inbound_without_enable_and_public_hosts() {
    let (_data, engine) = engine();
    let invalid = A2aPolicy {
        allow_inbound: true,
        ..A2aPolicy::default()
    };
    assert!(engine.a2a_set_policy(invalid).is_err());
    let mut public = A2aPolicy {
        enabled: true,
        bind_host: "0.0.0.0".into(),
        ..Default::default()
    };
    assert!(engine.a2a_set_policy(public.clone()).is_err());
    public.bind_host = "127.0.0.1".into();
    assert!(engine.a2a_set_policy(public).is_ok());
}

#[tokio::test]
async fn a2a_inbound_and_task_caps_fail_closed_and_are_not_advertised() {
    let (_data, engine) = engine();
    let inbound = A2aPolicy {
        enabled: true,
        allow_inbound: true,
        port: 0,
        max_tasks: 2,
        task_ttl_secs: 60,
        ..Default::default()
    };
    let error = engine.a2a_start(true, Some(inbound)).await.unwrap_err();
    assert!(
        error.to_string().contains("capability contract"),
        "unexpected error: {error}"
    );
    assert!(!engine.a2a_status().running);

    let started = engine
        .a2a_start(
            true,
            Some(A2aPolicy {
                enabled: true,
                allow_inbound: false,
                port: 0,
                max_tasks: 2,
                task_ttl_secs: 60,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert!(!started.status.inbound_enabled);
    assert_eq!(engine.a2a_tasks(200, None).tasks.len(), 0);
    let stopped = engine.a2a_stop(Some("operator stop")).await.unwrap();
    assert!(stopped.last_error.is_none());
    assert!(engine.a2a_tasks(10, None).tasks.is_empty());
}

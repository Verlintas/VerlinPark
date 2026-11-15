use vp_a2a::{
    A2aLimits, A2aPolicy, A2aServer, A2aTaskUpdate, AgentCardSpec, Message, Part, TaskState,
};

#[test]
fn policy_requires_enable_and_one_token() {
    let mut policy = A2aPolicy::default();
    assert!(!policy.is_enabled());
    assert!(policy.validate().is_err());
    policy = policy.enable();
    assert!(policy.validate().is_err(), "no token must fail closed");
    policy = policy.with_bearer_token("token-0123456789").unwrap();
    assert!(policy.validate().is_ok());
    assert!(policy.bearer_token_configured());
}

#[test]
fn token_bounds_are_enforced() {
    assert!(A2aPolicy::new("a", "b", "1").with_bearer_token("").is_err());
    assert!(
        A2aPolicy::new("a", "b", "1")
            .with_bearer_token("x".repeat(100_000))
            .is_err()
    );
    let secret = vp_a2a::A2aSecretToken::new("short").unwrap();
    let policy = A2aPolicy::new("a", "b", "1")
        .with_bearer_secret(secret)
        .enable();
    assert!(policy.validate().is_ok());
    let debug = format!("{policy:?}");
    assert!(!debug.contains("short"));
}

#[test]
fn limits_validate_and_bound_messages() {
    let limits = A2aLimits::default();
    assert!(limits.validate().is_ok());
    let message = Message::user_text("m1", "hello");
    assert!(limits.validate_message(&message).is_ok());
    let too_many = Message::new(
        vp_a2a::MessageRole::User,
        "m2",
        (0..100)
            .map(|index| Part::text(format!("p{index}")))
            .collect(),
    );
    assert!(limits.validate_message(&too_many).is_err());
}

#[test]
fn agent_card_spec_is_bounded_and_loopback_agnostic() {
    let spec = AgentCardSpec::new("name", "description", "1.0.0");
    assert_eq!(spec.name, "name");
    assert_eq!(spec.version, "1.0.0");
    let mut policy = A2aPolicy::default().with_card_spec(spec);
    policy = policy.with_listen("127.0.0.1:0").unwrap();
    assert_eq!(policy.listen_addr().to_string(), "127.0.0.1:0");
    assert!(
        policy.validate().is_err(),
        "disabled policy fails validation"
    );
}

#[test]
fn loopback_parser_rejects_wildcards_and_public_hosts() {
    for address in ["0.0.0.0:0", "192.168.1.4:9000", "[::]:0"] {
        assert!(
            A2aPolicy::default().with_listen(address).is_err(),
            "{address} must be rejected"
        );
    }
    for address in ["127.0.0.1:0", "[::1]:0"] {
        assert!(
            A2aPolicy::default().with_listen(address).is_ok(),
            "{address} must be accepted"
        );
    }
}

#[tokio::test]
async fn handler_cannot_be_started_without_enable_or_token() {
    let result = A2aServer::start(A2aPolicy::default(), |_request| async {
        Ok::<_, vp_a2a::A2aError>(A2aTaskUpdate::new(TaskState::Rejected))
    })
    .await;
    assert!(result.is_err());
}

#[test]
fn task_state_strings_are_stable() {
    for (state, label) in [
        (TaskState::Submitted, "submitted"),
        (TaskState::Working, "working"),
        (TaskState::InputRequired, "input_required"),
        (TaskState::AuthRequired, "auth_required"),
        (TaskState::Completed, "completed"),
        (TaskState::Failed, "failed"),
        (TaskState::Canceled, "canceled"),
        (TaskState::Rejected, "rejected"),
    ] {
        assert_eq!(state.as_str(), label);
    }
}

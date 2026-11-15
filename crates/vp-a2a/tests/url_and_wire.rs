//! Public-boundary tests for loopback URL parsing and the bounded wire model.
//!
//! These tests intentionally use only the crate's public API. They complement
//! the unit tests inside `url.rs`, `wire.rs`, and `policy.rs` by testing the
//! same invariants from a consumer's point of view.

use std::str::FromStr;

use serde_json::{Map, Value};
use vp_a2a::{
    A2aLimits, A2aPolicy, A2aTask, A2aUrl, AgentCard, AgentProvider, AgentSkill, Artifact,
    FileContent, Message, MessageRole, Part, TaskState, TaskStatus, validate_loopback_http_url,
};

#[test]
fn url_accepts_only_literal_loopback_and_preserves_authority() {
    for (input, expected) in [
        ("http://127.0.0.1:4310", "http://127.0.0.1:4310"),
        ("HTTP://127.0.0.1:4310", "http://127.0.0.1:4310"),
        ("https://127.0.0.1:443", "https://127.0.0.1:443"),
        ("http://127.0.0.1", "http://127.0.0.1:80"),
        ("https://127.0.0.1", "https://127.0.0.1:443"),
        ("http://[::1]:9000", "http://[::1]:9000"),
        ("http://[::1]", "http://[::1]:80"),
    ] {
        let url = A2aUrl::parse(input).unwrap_or_else(|error| panic!("{input}: {error}"));
        assert_eq!(url.to_string(), expected);
        assert_eq!(url.base(), expected);
        assert_eq!(
            url.join("/.well-known/agent-card.json"),
            format!("{expected}/.well-known/agent-card.json")
        );
        assert_eq!(url.join("a2a"), format!("{expected}/a2a"));
    }
}

#[test]
fn url_rejects_public_hosts_hostnames_and_ambiguous_forms() {
    for input in [
        "",
        " http://127.0.0.1:9000",
        "http://127.0.0.1:9000 ",
        "http://127.0.0.1:9000/ ",
        "http://example.com:9000",
        "http://localhost:9000",
        "http://0.0.0.0:9000",
        "http://[::]:9000",
        "http://192.168.1.10:9000",
        "http://10.0.0.1:9000",
        "http://127.0.0.2:9000",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:not-a-port",
        "ftp://127.0.0.1:9000",
        "ws://127.0.0.1:9000",
        "127.0.0.1:9000",
        "http://user:pass@127.0.0.1:9000",
        "http://127.0.0.1:9000/path",
        "http://127.0.0.1:9000/?query=1",
        "http://127.0.0.1:9000/#fragment",
        "http://[::1:9000",
        "http://::1:9000",
        "http://127.0.0.1:9000\n",
    ] {
        assert!(A2aUrl::parse(input).is_err(), "{input} must be rejected");
    }
}

#[test]
fn url_validation_allows_card_paths_but_not_control_or_query_suffixes() {
    assert!(validate_loopback_http_url("http://127.0.0.1:9000").is_ok());
    assert!(validate_loopback_http_url("http://127.0.0.1:9000/agent-card.json").is_ok());
    assert!(validate_loopback_http_url("http://[::1]:9000/a2a/card").is_ok());
    assert!(validate_loopback_http_url("http://127.0.0.1:9000/a/b/c").is_ok());
    for input in [
        "http://127.0.0.1:9000/card?x=1",
        "http://127.0.0.1:9000/card#fragment",
        "http://user@127.0.0.1:9000/card",
        "http://127.0.0.1:9000/card\u{0007}",
        "http://example.com/card",
    ] {
        assert!(
            validate_loopback_http_url(input).is_err(),
            "{input} must be rejected"
        );
    }
}

#[test]
fn url_parsing_is_total_for_bounded_adversarial_inputs() {
    let inputs = [
        "http://",
        "://",
        "http:/127.0.0.1",
        "http://[",
        "http://[]",
        "http://[]:1",
        "http://127.0.0.1:",
        "http://127.0.0.1:+1",
        "http://127.0.0.1:-1",
        "http://127.0.0.1:1/",
        "http://127.0.0.1:1//",
        "HTTP://[0:0:0:0:0:0:0:1]:7777",
        "http://127.0.0.1:65535",
        "https://[::1]:1/",
    ];
    for input in inputs {
        let parsed = A2aUrl::parse(input);
        if let Ok(url) = parsed {
            assert!(url.port() > 0);
            assert!(url.to_string().starts_with("http"));
            assert!(!url.to_string().contains('@'));
        }
    }
}

#[test]
fn from_str_and_listen_address_agree_on_loopback_policy() {
    let parsed = A2aUrl::from_str("http://127.0.0.1:1234").unwrap();
    assert_eq!(parsed.port(), 1234);
    let listen = vp_a2a::A2aListenAddr::from_str("127.0.0.1:1234").unwrap();
    assert_eq!(listen.port(), 1234);
    assert!(vp_a2a::A2aListenAddr::from_str("0.0.0.0:1234").is_err());
    assert!(vp_a2a::A2aListenAddr::from_str("192.168.0.2:1234").is_err());
    assert!(vp_a2a::A2aListenAddr::from_str("[::1]:1234").is_ok());
    assert!(vp_a2a::A2aListenAddr::from_str("[::]:1234").is_err());
    assert!(vp_a2a::A2aListenAddr::from_str("localhost:1234").is_err());
}

#[test]
fn agent_card_requires_clean_loopback_and_unique_skills() {
    let mut card = AgentCard::new(
        "VerlinPark",
        "loopback facade",
        "http://127.0.0.1:9000/agent-card.json",
        "0.1.0",
    );
    card.capabilities.streaming = true;
    card.default_input_modes = vec!["text/plain".into()];
    card.default_output_modes = vec!["text/plain".into()];
    card.skills = vec![
        AgentSkill::new("scout", "Scout", "read-only exploration"),
        AgentSkill::new("verify", "Verify", "evidence review"),
    ];
    card.provider = Some(AgentProvider::new("VerlinPark"));
    assert!(card.validate(&A2aLimits::default()).is_ok());

    let encoded = card.encode().unwrap();
    let decoded = AgentCard::from_json(encoded.as_bytes()).unwrap();
    assert_eq!(decoded, card);

    let mut bad_url = card.clone();
    bad_url.url = "http://example.com/agent-card.json".into();
    assert!(bad_url.validate(&A2aLimits::default()).is_err());
    let mut duplicate = card.clone();
    duplicate.skills[1].id = "scout".into();
    assert!(duplicate.validate(&A2aLimits::default()).is_err());
    let mut bad_name = card;
    bad_name.name = "bad\u{0007}name".into();
    assert!(bad_name.validate(&A2aLimits::default()).is_err());
}

#[test]
fn agent_card_decode_rejects_oversized_and_invalid_json() {
    let limits = A2aLimits {
        max_body_bytes: 4,
        ..Default::default()
    };
    let error = AgentCard::decode(br#"{"name":"a"}"#, &limits).unwrap_err();
    assert!(matches!(error, vp_a2a::A2aError::BoundsExceeded { .. }));
    assert!(AgentCard::decode(b"not-json", &A2aLimits::default()).is_err());
    assert!(AgentCard::from_json(br#"{"name":"only-name"}"#).is_err());

    let card = AgentCard::new("card", "description", "http://127.0.0.1:9000", "1");
    let value = serde_json::to_value(&card).unwrap();
    let invalid_protocol = serde_json::json!({
        "name": value["name"],
        "description": value["description"],
        "url": value["url"],
        "version": value["version"],
        "protocolVersion": "",
    });
    assert!(
        AgentCard::from_json(invalid_protocol.to_string().as_bytes()).is_err(),
        "empty protocol version must fail"
    );
}

#[test]
fn message_constructors_are_canonical_and_bounded() {
    let message = Message::user_text("message-1", "hello");
    assert_eq!(message.role, MessageRole::User);
    assert_eq!(message.kind, "message");
    assert_eq!(message.parts, vec![Part::text("hello")]);
    assert!(message.validate(&A2aLimits::default()).is_ok());

    let agent = Message::agent_text("message-2", "answer")
        .with_context("context-1")
        .with_task("task-1");
    assert_eq!(agent.role, MessageRole::Agent);
    assert_eq!(agent.context_id.as_deref(), Some("context-1"));
    assert_eq!(agent.task_id.as_deref(), Some("task-1"));
    assert!(agent.validate(&A2aLimits::default()).is_ok());

    let json = serde_json::to_value(&message).unwrap();
    assert_eq!(json["kind"], "message");
    assert_eq!(json["role"], "user");
    assert_eq!(json["messageId"], "message-1");
    assert_eq!(json["parts"][0]["kind"], "text");
    assert_eq!(json["parts"][0]["text"], "hello");
}

#[test]
fn message_and_part_bounds_are_checked_before_execution() {
    let limits = A2aLimits::default();
    let mut empty = Message::user_text("m1", "x");
    empty.parts.clear();
    assert!(empty.validate(&limits).is_err());

    let mut too_many = Message::user_text("m2", "x");
    too_many.parts = (0..limits.max_message_parts + 1)
        .map(|index| Part::text(format!("part-{index}")))
        .collect();
    assert!(too_many.validate(&limits).is_err());

    let oversized = Message::user_text("m3", "x".repeat(limits.max_part_text_bytes + 1));
    assert!(oversized.validate(&limits).is_err());

    let mut control = Message::user_text("m4", "x");
    control.message_id = "bad\nid".into();
    assert!(control.validate(&limits).is_err());

    let mut wrong_kind = Message::user_text("m5", "x");
    wrong_kind.kind = "task".into();
    assert!(wrong_kind.validate(&limits).is_err());
}

#[test]
fn data_parts_are_bounded_maps_and_never_evaluated() {
    let mut map = Map::new();
    map.insert("key".into(), Value::String("value".into()));
    let part = Part::data(map.clone());
    assert_eq!(part.kind(), "data");
    let message = Message::new(MessageRole::User, "data-1", vec![part]);
    assert!(message.validate(&A2aLimits::default()).is_ok());

    let mut oversized = Map::new();
    oversized.insert("blob".into(), Value::String("x".repeat(200_000)));
    let message = Message::new(MessageRole::User, "data-2", vec![Part::data(oversized)]);
    assert!(message.validate(&A2aLimits::default()).is_err());
}

#[test]
fn file_parts_round_trip_bytes_without_dereferencing_uris() {
    let file = FileContent::from_bytes(
        Some("report.txt".into()),
        Some("text/plain".into()),
        b"hello artifact",
    );
    assert_eq!(
        file.decoded_bytes().unwrap().as_deref(),
        Some(b"hello artifact".as_slice())
    );
    let uri_file = FileContent::from_uri(
        Some("remote.txt".into()),
        Some("text/plain".into()),
        "file:///etc/passwd",
    );
    assert!(uri_file.decoded_bytes().unwrap().is_none());
    let part = Part::file(file);
    assert_eq!(part.kind(), "file");
    assert!(Part::file(uri_file).kind() == "file");
}

#[test]
fn artifacts_require_identity_parts_and_bounded_metadata() {
    let artifact = Artifact::new("artifact-1", vec![Part::text("body")]);
    assert!(artifact.validate(&A2aLimits::default()).is_ok());

    let mut bad_id = artifact.clone();
    bad_id.artifact_id = String::new();
    assert!(bad_id.validate(&A2aLimits::default()).is_err());

    let mut no_parts = artifact.clone();
    no_parts.parts.clear();
    assert!(no_parts.validate(&A2aLimits::default()).is_err());

    let mut oversized = artifact;
    oversized.parts = vec![Part::text("x".repeat(300_000))];
    assert!(oversized.validate(&A2aLimits::default()).is_err());
}

#[test]
fn task_projection_requires_task_kind_and_identity() {
    let task = A2aTask::new("task-1", "context-1", TaskState::Submitted);
    assert_eq!(task.kind, "task");
    assert!(task.validate(&A2aLimits::default()).is_ok());

    let mut bad_id = task.clone();
    bad_id.id = String::new();
    assert!(bad_id.validate(&A2aLimits::default()).is_err());

    let mut bad_context = task.clone();
    bad_context.context_id = "bad\tcontext".into();
    assert!(bad_context.validate(&A2aLimits::default()).is_err());

    let mut wrong_kind = task;
    wrong_kind.kind = "message".into();
    assert!(wrong_kind.validate(&A2aLimits::default()).is_err());
}

#[test]
fn task_states_serialize_canonically_and_accept_a2a_hyphens() {
    for (state, canonical) in [
        (TaskState::Submitted, "submitted"),
        (TaskState::Working, "working"),
        (TaskState::InputRequired, "input_required"),
        (TaskState::AuthRequired, "auth_required"),
        (TaskState::Completed, "completed"),
        (TaskState::Failed, "failed"),
        (TaskState::Canceled, "canceled"),
        (TaskState::Rejected, "rejected"),
    ] {
        let json = serde_json::to_value(state).unwrap();
        assert_eq!(json, canonical);
        let decoded: TaskState = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, state);
    }
    assert_eq!(
        serde_json::from_value::<TaskState>(Value::String("input-required".into())).unwrap(),
        TaskState::InputRequired
    );
    assert_eq!(
        serde_json::from_value::<TaskState>(Value::String("auth-required".into())).unwrap(),
        TaskState::AuthRequired
    );
    assert!(serde_json::from_value::<TaskState>(Value::String("running".into())).is_err());
}

#[test]
fn task_status_and_task_are_camel_case_on_the_wire() {
    let status = TaskStatus::new(TaskState::Completed)
        .with_message(Message::agent_text("agent-message", "done"));
    let task = A2aTask {
        status,
        history: vec![Message::user_text("user-message", "work")],
        ..A2aTask::new("task-9", "context-9", TaskState::Working)
    };
    let json = serde_json::to_value(&task).unwrap();
    assert_eq!(json["contextId"], "context-9");
    assert_eq!(json["status"]["state"], "completed");
    assert_eq!(json["status"]["message"]["role"], "agent");
    assert_eq!(json["history"][0]["messageId"], "user-message");
    assert!(json.get("context_id").is_none());
}

#[test]
fn policy_accepts_generated_tokens_but_never_prints_them() {
    let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let policy = A2aPolicy::new("card", "description", "1")
        .with_bearer_token(token)
        .unwrap()
        .with_listen("127.0.0.1:0")
        .unwrap()
        .enable();
    assert!(policy.validate().is_ok());
    assert!(policy.is_enabled());
    let debug = format!("{policy:?}");
    assert!(!debug.contains(token));
    assert!(!debug.contains("0123456789abcdef0123456789abcdef"));
}

#[test]
fn policy_never_widens_to_public_hosts_after_clone() {
    let policy = A2aPolicy::new("card", "description", "1")
        .with_bearer_token("0123456789abcdef")
        .unwrap()
        .with_listen("127.0.0.1:0")
        .unwrap()
        .enable();
    assert!(policy.clone().with_listen("0.0.0.0:0").is_err());
    assert!(policy.clone().with_listen("[::]:0").is_err());
    assert!(policy.validate().is_ok());
}

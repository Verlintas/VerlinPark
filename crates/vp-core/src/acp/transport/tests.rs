use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use vp_protocol::types::AcpTransport;

use super::*;

fn launch(transport: AcpTransport, endpoint: &str) -> AcpLaunch {
    AcpLaunch {
        name: "remote-test".into(),
        command: String::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        cwd: PathBuf::from("/tmp/acp"),
        project_root: PathBuf::from("/tmp/acp"),
        sandbox_root: PathBuf::from("/tmp/acp-sandbox"),
        secret_roots: Vec::new(),
        network_allowed: true,
        home: PathBuf::from("/tmp"),
        restrict_home: false,
        read_jail: false,
        git_branch: None,
        mcp_servers: Vec::new(),
        timeout: Duration::from_secs(60),
        transport,
        endpoint: Some(endpoint.into()),
        auth_header_env: None,
        headers: BTreeMap::new(),
        connect_timeout: Duration::from_secs(5),
        allow_remote: true,
        resume_session_id: None,
        diagnostics: None,
    }
}

#[test]
fn decodes_complete_sse_events() {
    let mut decoder = SseDecoder::new(1024);
    let events = decoder
        .push(b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1}\n\n")
        .unwrap();
    assert_eq!(events, vec![r#"{"jsonrpc":"2.0","id":1}"#]);
    assert!(decoder.push(b": keepalive\n\n").unwrap().is_empty());
}

#[test]
fn decodes_multiline_data_and_crlf() {
    let mut decoder = SseDecoder::new(1024);
    let events = decoder
        .push(b"data: {\"jsonrpc\":\"2.0\",\r\ndata: \"id\":1,\"method\":\"initialize\"}\r\n\r\n")
        .unwrap();
    // SSE joins data lines with '\n', which is valid between JSON tokens.
    assert_eq!(events.len(), 1);
    let parsed = parse_message_text(&events[0], 1024).unwrap();
    assert!(matches!(parsed, RpcMessage::Request(_)));
}

#[test]
fn handles_chunk_boundaries_inside_lines_and_utf8() {
    let mut decoder = SseDecoder::new(1024);
    assert!(
        decoder
            .push("data: {\"text\":\"".as_bytes())
            .unwrap()
            .is_empty()
    );
    assert!(decoder.push("héllo".as_bytes()).unwrap().is_empty());
    let events = decoder.push("\"}\n\n".as_bytes()).unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("héllo"));
}

#[test]
fn dispatches_a_trailing_event_without_a_blank_line() {
    let mut decoder = SseDecoder::new(1024);
    assert!(decoder.push(b"data: {\"ok\":true}").unwrap().is_empty());
    let events = decoder.finish().unwrap();
    assert_eq!(events, vec![r#"{"ok":true}"#]);
}

#[test]
fn rejects_oversized_sse_lines_and_events() {
    let mut decoder = SseDecoder::new(16);
    let error = decoder
        .push(b"data: this-is-more-than-sixteen-bytes")
        .unwrap_err();
    assert!(error.contains("exceeded"), "{error}");

    let mut decoder = SseDecoder::new(20);
    let error = decoder
        .push(b"data: 1234567890\ndata: 1234567890\n\n")
        .unwrap_err();
    assert!(error.contains("event exceeded"), "{error}");
}

#[test]
fn strips_a_split_utf8_bom() {
    let mut decoder = SseDecoder::new(1024);
    assert!(decoder.push(&[0xEF]).unwrap().is_empty());
    assert!(decoder.push(&[0xBB]).unwrap().is_empty());
    let events = decoder
        .push(&[
            0xBF, b'd', b'a', b't', b'a', b':', b' ', b'{', b'}', b'\n', b'\n',
        ])
        .unwrap();
    assert_eq!(events, vec!["{}"]);
}

#[test]
fn rejects_invalid_utf8_in_an_sse_line() {
    let mut decoder = SseDecoder::new(1024);
    let error = decoder.push(b"data: \xff\n\n").unwrap_err();
    assert!(error.contains("UTF-8"), "{error}");
}

#[test]
fn dispatches_multiple_events_from_one_chunk() {
    let mut decoder = SseDecoder::new(1024);
    let events = decoder
        .push(b"data: one\n\ndata: two\n\ndata: three\n\n")
        .unwrap();
    assert_eq!(events, vec!["one", "two", "three"]);
}

#[test]
fn ignores_comment_only_events() {
    let mut decoder = SseDecoder::new(1024);
    assert!(decoder.push(b": keepalive\n\n").unwrap().is_empty());
    assert!(decoder.finish().unwrap().is_empty());
}

#[test]
fn accepts_an_event_at_the_exact_boundary() {
    let payload = "x".repeat(20);
    let mut decoder = SseDecoder::new(21); // "data: " + 20 bytes
    let events = decoder
        .push(format!("data: {payload}\n\n").as_bytes())
        .unwrap();
    assert_eq!(events, vec![payload]);
}

#[test]
fn rejects_an_overlong_event_name() {
    let mut decoder = SseDecoder::new(1024);
    let name = "n".repeat(257);
    let error = decoder
        .push(format!("event: {name}\n\n").as_bytes())
        .unwrap_err();
    assert!(error.contains("event name"), "{error}");
}

#[test]
fn a_field_without_a_colon_is_treated_as_an_empty_value() {
    let mut decoder = SseDecoder::new(1024);
    // The bare `data` line produces an empty event, which is dropped; the
    // following valid event is still dispatched.
    let events = decoder.push(b"data\n\ndata: x\n\n").unwrap();
    assert_eq!(events, vec!["x"]);
    assert!(decoder.push(b"data\n\n").unwrap().is_empty());
}

#[test]
fn ignores_sse_id_and_retry_fields() {
    let mut decoder = SseDecoder::new(1024);
    let events = decoder
        .push(b"id: 42\nretry: 1000\ndata: {\"a\":1}\n\n")
        .unwrap();
    assert_eq!(events, vec![r#"{"a":1}"#]);
}

#[test]
fn parses_requests_notifications_and_responses() {
    let request = parse_message_text(
        r#"{"jsonrpc":"2.0","id":"a","method":"session/new","params":{}}"#,
        1024,
    )
    .unwrap();
    assert!(matches!(request, RpcMessage::Request(_)));

    let notification = parse_message_text(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#,
        1024,
    )
    .unwrap();
    assert!(matches!(notification, RpcMessage::Request(_)));

    let response = parse_message_text(
        r#"{"jsonrpc":"2.0","id":7,"result":{"sessionId":"s"}}"#,
        1024,
    )
    .unwrap();
    assert!(matches!(response, RpcMessage::Response(_)));
}

#[test]
fn rejects_batches_malformed_and_ambiguous_messages() {
    for text in [
        r#"[{"jsonrpc":"2.0","id":1,"method":"initialize"}]"#,
        "not json",
        "{}",
        r#"{"jsonrpc":"2.0","id":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"  "}"#,
    ] {
        assert!(
            parse_message_text(text, 1024).is_err(),
            "{text} should be rejected"
        );
    }
    assert!(parse_message_text("{}", 1).is_err());
}

#[test]
fn session_header_rules_match_the_rfd() {
    for method in [
        "session/prompt",
        "session/cancel",
        "session/load",
        "session/set_mode",
        "session/set_config_option",
    ] {
        assert!(method_requires_session_header(method), "{method}");
        assert!(require_session_for_method(method, None).is_err());
        assert!(require_session_for_method(method, Some("s")).is_ok());
    }
    for method in ["initialize", "session/new", "session/list"] {
        assert!(!method_requires_session_header(method), "{method}");
        assert!(require_session_for_method(method, None).is_ok());
    }
}

#[test]
fn remote_config_requires_opt_in_and_valid_scheme() {
    let mut value = launch(AcpTransport::StreamableHttp, "https://acp.example.test/acp");
    value.allow_remote = false;
    let error = RemoteTransportConfig::from_launch(&value)
        .unwrap_err()
        .to_string();
    assert!(error.contains("allow_remote"), "{error}");

    let mut value = launch(AcpTransport::StreamableHttp, "wss://acp.example.test/acp");
    value.allow_remote = true;
    assert!(RemoteTransportConfig::from_launch(&value).is_err());

    let value = launch(AcpTransport::Websocket, "wss://acp.example.test/acp");
    let config = RemoteTransportConfig::from_launch(&value).unwrap();
    assert_eq!(config.transport, AcpTransport::Websocket);
    assert_eq!(config.endpoint, "wss://acp.example.test/acp");
    assert_eq!(config.connect_timeout, Duration::from_secs(5));
    assert_eq!(config.max_message_bytes, DEFAULT_MAX_MESSAGE_BYTES);
}

#[test]
fn remote_config_resolves_headers_without_leaking_values() {
    // Use a test-only variable name that cannot collide with daemon secrets.
    unsafe {
        std::env::set_var("VERLINPARK_TEST_ACP_TRANSPORT_TOKEN", "Bearer test-value");
    }
    let mut value = launch(AcpTransport::StreamableHttp, "https://acp.example.test/acp");
    value.auth_header_env = Some("VERLINPARK_TEST_ACP_TRANSPORT_TOKEN".into());
    value.headers = BTreeMap::from([("x-project".into(), "demo".into())]);
    let config = RemoteTransportConfig::from_launch(&value).unwrap();
    let debug = format!("{config:?}");
    assert!(debug.contains("authorization"));
    assert!(debug.contains("x-project"));
    assert!(
        !debug.contains("test-value"),
        "secret leaked in Debug: {debug}"
    );
    assert_eq!(config.headers.len(), 2);
    unsafe {
        std::env::remove_var("VERLINPARK_TEST_ACP_TRANSPORT_TOKEN");
    }
}

#[test]
fn remote_config_fails_closed_when_auth_env_is_missing() {
    let mut value = launch(AcpTransport::StreamableHttp, "https://acp.example.test/acp");
    value.auth_header_env = Some("VERLINPARK_TEST_MISSING_ACP_TOKEN".into());
    let error = RemoteTransportConfig::from_launch(&value)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("VERLINPARK_TEST_MISSING_ACP_TOKEN"),
        "{error}"
    );
    assert!(!error.contains("Bearer"));
}

#[test]
fn connect_time_revalidates_hand_edited_headers_and_timeout() {
    let mut value = launch(AcpTransport::StreamableHttp, "https://acp.example.test/acp");
    value.headers = BTreeMap::from([("authorization".into(), "Bearer inline".into())]);
    assert!(RemoteTransportConfig::from_launch(&value).is_err());

    value.headers = BTreeMap::from([("x-project".into(), "demo".into())]);
    value.connect_timeout = Duration::from_secs(9_999);
    let config = RemoteTransportConfig::from_launch(&value).unwrap();
    assert_eq!(config.connect_timeout, Duration::from_secs(300));
}

#[test]
fn stdio_config_is_not_a_remote_transport() {
    let value = launch(AcpTransport::Stdio, "https://acp.example.test/acp");
    let error = RemoteTransportConfig::from_launch(&value)
        .unwrap_err()
        .to_string();
    assert!(error.contains("stdio"), "{error}");
}

#[test]
fn message_size_is_bounded_in_both_directions() {
    let message =
        parse_message_text(r#"{"jsonrpc":"2.0","id":1,"method":"session/new"}"#, 1024).unwrap();
    assert!(serialize_message(&message, 8).is_err());
    assert!(serialize_message(&message, 1024).is_ok());
    assert!(parse_message_text(r#"{"jsonrpc":"2.0","id":1,"method":"session/new"}"#, 8).is_err());
}

#[test]
fn id_keys_distinguish_string_and_numeric_ids() {
    assert_ne!(id_key(&Id::String("1".into())), id_key(&Id::Number(1)));
    assert_eq!(id_key(&Id::Null), "null");
}

#[test]
fn bounded_errors_strip_control_characters_and_cap_length() {
    let text = format!("line one\nline two\r{}", "x".repeat(2_000));
    let bounded = bounded_error(&text);
    assert!(!bounded.contains('\n'));
    assert!(bounded.len() <= 512 + "… (truncated)".len());
    assert!(bounded_error("界".repeat(1_000).as_str()).len() <= 512 + "… (truncated)".len());
}

#[test]
fn request_helpers_extract_methods_ids_and_sessions() {
    let request = parse_message_text(
        r#"{"jsonrpc":"2.0","id":"r1","method":"session/prompt","params":{"sessionId":"s1"}}"#,
        1024,
    )
    .unwrap();
    assert_eq!(message_method(&request), Some("session/prompt"));
    assert_eq!(message_id(&request), Some(Id::String("r1".into())));
    let RpcMessage::Request(request) = &request else {
        panic!("expected request");
    };
    assert_eq!(params_session_id(&request.params).as_deref(), Some("s1"));

    let response = parse_message_text(
        r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"s2"}}"#,
        1024,
    )
    .unwrap();
    let RpcMessage::Response(response) = &response else {
        panic!("expected response");
    };
    assert_eq!(response_session_id(response).as_deref(), Some("s2"));
}

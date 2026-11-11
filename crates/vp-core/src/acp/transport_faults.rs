//! Fault-injection tests for the remote ACP transport using wiremock.
//!
//! These cover initialization and stream-open failures that are easier to
//! express as fixed HTTP responses than through the stateful ACP harness.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::Client;
use agent_client_protocol::jsonrpcmsg::Message as RpcMessage;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use vp_protocol::types::AcpTransport;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{AcpHost, AcpLaunch, AcpPermissionDecision, AcpPermissionRequest, AcpUpdate};
use crate::acp::transport_integration::{TestServerConfig, TestServerHandle};
use crate::acp::{probe, run_prompt};

#[derive(Default)]
struct NoopHost {
    permissions: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl AcpHost for NoopHost {
    async fn update(&self, _update: AcpUpdate) {}

    async fn permission(&self, _request: AcpPermissionRequest) -> AcpPermissionDecision {
        self.permissions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        AcpPermissionDecision::Approve
    }
}

fn launch(endpoint: &str) -> AcpLaunch {
    AcpLaunch {
        name: "fault".into(),
        command: String::new(),
        args: Vec::new(),
        env: Default::default(),
        cwd: PathBuf::from("/tmp/verlinpark-acp-fault"),
        project_root: PathBuf::from("/tmp/verlinpark-acp-fault"),
        sandbox_root: PathBuf::from("/tmp/verlinpark-acp-fault-sandbox"),
        secret_roots: Vec::new(),
        network_allowed: true,
        home: PathBuf::from("/tmp"),
        restrict_home: false,
        read_jail: false,
        git_branch: None,
        mcp_servers: Vec::new(),
        timeout: Duration::from_secs(20),
        transport: AcpTransport::StreamableHttp,
        endpoint: Some(endpoint.into()),
        auth_header_env: None,
        headers: Default::default(),
        connect_timeout: Duration::from_secs(2),
        allow_remote: true,
        resume_session_id: None,
        diagnostics: None,
    }
}

fn initialize_body() -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": 1,
            "agentCapabilities": {},
            "agentInfo": {"name": "fault", "version": "0"},
            "connectionId": "conn-fault"
        }
    })
}

async fn run(endpoint: &str) -> anyhow::Result<super::AcpRunOutcome> {
    let host = Arc::new(NoopHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    run_prompt(launch(endpoint), "fault".into(), host, cancel_rx).await
}

#[tokio::test]
async fn initialize_http_401_is_reported_without_the_response_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(ResponseTemplate::new(401).set_body_string("echo Bearer should-not-leak"))
        .mount(&server)
        .await;
    let error = run(&format!("{}/acp", server.uri()))
        .await
        .expect_err("401 must fail");
    let text = error.to_string();
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains("should-not-leak"), "body leaked: {text}");
}

#[tokio::test]
async fn server_error_bodies_never_echo_authorization() {
    unsafe {
        std::env::set_var("VERLINPARK_TEST_ACP_FAULT_TOKEN", "Bearer reflected-secret");
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(
            ResponseTemplate::new(500).set_body_string("upstream said: Bearer reflected-secret"),
        )
        .mount(&server)
        .await;
    let mut launch = launch(&format!("{}/acp", server.uri()));
    launch.auth_header_env = Some("VERLINPARK_TEST_ACP_FAULT_TOKEN".into());
    let host = Arc::new(NoopHost::default());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let error = run_prompt(launch, "fault".into(), host, cancel_rx)
        .await
        .expect_err("500 must fail");
    let text = error.to_string();
    assert!(text.contains("500"), "{text}");
    assert!(!text.contains("reflected-secret"), "body leaked: {text}");
    unsafe {
        std::env::remove_var("VERLINPARK_TEST_ACP_FAULT_TOKEN");
    }
}

#[tokio::test]
async fn initialize_http_415_and_500_are_reported() {
    for status in [415_u16, 500] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/acp"))
            .respond_with(ResponseTemplate::new(status).set_body_string("nope"))
            .mount(&server)
            .await;
        let error = run(&format!("{}/acp", server.uri()))
            .await
            .expect_err("status must fail");
        assert!(error.to_string().contains(&status.to_string()), "{error}");
    }
}

#[tokio::test]
async fn initialize_batch_array_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Acp-Connection-Id", "conn-batch")
                .set_body_string("[]"),
        )
        .mount(&server)
        .await;
    let error = run(&format!("{}/acp", server.uri()))
        .await
        .expect_err("batch array must fail");
    assert!(
        error.to_string().to_lowercase().contains("batch"),
        "{error}"
    );
}

#[tokio::test]
async fn initialize_without_a_connection_id_is_rejected() {
    let server = MockServer::start().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"protocolVersion": 1, "agentCapabilities": {}}
    });
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let error = run(&format!("{}/acp", server.uri()))
        .await
        .expect_err("missing connection id must fail");
    assert!(
        error.to_string().to_lowercase().contains("connection"),
        "{error}"
    );
}

#[tokio::test]
async fn initialize_malformed_json_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Acp-Connection-Id", "conn-bad")
                .set_body_string("not-json"),
        )
        .mount(&server)
        .await;
    let error = run(&format!("{}/acp", server.uri()))
        .await
        .expect_err("malformed initialize must fail");
    assert!(error.to_string().to_lowercase().contains("json"), "{error}");
}

#[tokio::test]
async fn initialize_protocol_error_is_surfaced() {
    let server = MockServer::start().await;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32000, "message": "authentication required"}
    });
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Acp-Connection-Id", "conn-error")
                .set_body_json(body),
        )
        .mount(&server)
        .await;
    let error = run(&format!("{}/acp", server.uri()))
        .await
        .expect_err("protocol error must fail");
    assert!(
        error.to_string().to_lowercase().contains("initialize"),
        "{error}"
    );
}

#[tokio::test]
async fn connection_stream_http_error_and_wrong_content_type_are_reported() {
    for (status, content_type, expected) in [
        (500_u16, "text/event-stream", "500"),
        (200, "application/json", "content type"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/acp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Acp-Connection-Id", "conn-get")
                    .set_body_json(initialize_body()),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/acp"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("Content-Type", content_type)
                    .set_body_string("stream failure"),
            )
            .mount(&server)
            .await;
        let error = run(&format!("{}/acp", server.uri()))
            .await
            .expect_err("stream open must fail");
        assert!(
            error.to_string().contains(expected),
            "expected {expected}: {error}"
        );
    }
}

#[tokio::test]
async fn probe_uses_the_same_transport_and_fails_the_same_way() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/acp"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let error = probe(launch(&format!("{}/acp", server.uri())))
        .await
        .expect_err("probe must fail");
    assert!(error.to_string().contains("503"), "{error}");
}

#[tokio::test]
async fn unreachable_endpoint_is_reported() {
    // Bind and immediately drop a listener to get a likely-closed local port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let error = run(&format!("http://{address}/acp"))
        .await
        .expect_err("closed port must fail");
    assert!(!error.to_string().is_empty());
}

async fn raw_http_channel(
    endpoint: &str,
    mutate: impl FnOnce(&mut crate::acp::RemoteTransportConfig),
) -> (
    agent_client_protocol::Channel,
    tokio::task::JoinHandle<std::result::Result<(), agent_client_protocol::Error>>,
) {
    let mut config =
        crate::acp::RemoteTransportConfig::from_launch(&launch(endpoint)).expect("config");
    mutate(&mut config);
    let (channel, future) = agent_client_protocol::ConnectTo::<Client>::into_channel_and_future(
        crate::acp::RemoteTransport::new(config),
    );
    (channel, tokio::spawn(future))
}

fn initialize_message() -> RpcMessage {
    serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {}}
    }))
    .expect("initialize")
}

fn session_new_message() -> RpcMessage {
    serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": "/tmp", "mcpServers": []}
    }))
    .expect("session/new")
}

#[tokio::test]
async fn idle_connection_stream_fails_with_a_bounded_error() {
    let handle = TestServerHandle::start(TestServerConfig::default()).await;
    let (mut channel, driver) = raw_http_channel(&handle.endpoint, |config| {
        config.stream_idle_timeout = Some(Duration::from_millis(100));
    })
    .await;
    let task = tokio::spawn(driver);
    channel.tx.unbounded_send(Ok(initialize_message())).unwrap();
    let mut error = String::new();
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(3), channel.rx.next()).await {
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(value))) => {
                error = value.to_string();
                break;
            }
            _ => break,
        }
    }
    task.abort();
    assert!(error.to_lowercase().contains("idle"), "{error}");
}

#[tokio::test]
async fn oversized_sse_event_fails_with_a_bounded_error() {
    let handle = TestServerHandle::start(TestServerConfig::default()).await;
    let (mut channel, driver) = raw_http_channel(&handle.endpoint, |config| {
        config.max_sse_event_bytes = 32;
    })
    .await;
    let task = tokio::spawn(driver);
    channel.tx.unbounded_send(Ok(initialize_message())).unwrap();
    // Wait for the initialize response and the connection stream to settle,
    // then request a session; its response event exceeds 32 bytes.
    let mut saw_initialize = false;
    let mut error = String::new();
    for _ in 0..6 {
        match tokio::time::timeout(Duration::from_secs(3), channel.rx.next()).await {
            Ok(Some(Ok(message))) => {
                if !saw_initialize {
                    saw_initialize = true;
                    channel
                        .tx
                        .unbounded_send(Ok(session_new_message()))
                        .unwrap();
                }
                let _ = message;
            }
            Ok(Some(Err(value))) => {
                error = value.to_string();
                break;
            }
            _ => break,
        }
    }
    task.abort();
    let lower = error.to_lowercase();
    assert!(
        lower.contains("exceeded") || lower.contains("malformed") || lower.contains("sse"),
        "{error}"
    );
}

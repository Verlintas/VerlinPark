//! End-to-end stdio test for the packaged remote helper.
//!
//! The daemon normally reaches this binary over SSH. Here the test launches the
//! same executable directly with `StdioTransport`, performs the real handshake,
//! and pages a Git bundle through bounded JSON-RPC frames. This covers the
//! transport, framing, capability negotiation, helper Git invocation, SHA-256
//! report, byte cap, and cleanup that an in-process unit test cannot.

use std::fs;
use std::path::Path;
use std::process::Command;

use base64::Engine as Base64Engine;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use vp_remote::protocol::*;
use vp_remote::{RemoteClient, RemoteClientConfig, RemoteTransport, StdioTransport};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const TOKEN_ENV: &str = "VERLINPARK_STDIO_TEST_TOKEN";

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let payload = (0..4096)
        .map(|index| format!("line-{index:04}-{}\n", "y".repeat(8)))
        .collect::<String>();
    fs::write(dir.path().join("payload.txt"), payload).unwrap();
    git(dir.path(), &["add", "a.txt", "payload.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    dir
}

async fn helper(root: &Path) -> RemoteClient {
    let transport = StdioTransport::new(env!("CARGO_BIN_EXE_vp-remote-agent"))
        .arg("remote-agent")
        .arg("--root")
        .arg(root.to_string_lossy().to_string())
        .arg("--token-env")
        .arg(TOKEN_ENV)
        .arg("--no-ledger")
        .env(TOKEN_ENV, TOKEN)
        .described_as("test helper");
    RemoteClient::connect(
        &transport as &dyn RemoteTransport,
        HelloParams {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            client_version: "stdio-test".into(),
            auth_token: TOKEN.into(),
            root: root.canonicalize().unwrap().to_string_lossy().to_string(),
        },
        RemoteClientConfig {
            request_timeout: std::time::Duration::from_secs(30),
            handshake_timeout: std::time::Duration::from_secs(10),
            max_frame_bytes: MAX_FRAME_BYTES as usize,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn packaged_helper_streams_a_bundle_over_stdio() {
    let dir = repo();
    let client = helper(dir.path()).await;
    assert!(client.supports_git_bundle());
    assert!(client.hello().has_capability(CAP_GIT_BUNDLE));
    let created = client
        .git_bundle_create(&GitBundleCreateParams {
            branch: "main".into(),
            base: None,
            max_bytes: 8 * 1024 * 1024,
            chunk_bytes: 1024,
            operation_id: Some("stdio-create".into()),
        })
        .await
        .unwrap();
    assert!(created.chunks > 1);
    let mut offset = 0u64;
    let mut hasher = Sha256::new();
    let mut frames = 0u32;
    loop {
        let chunk = client
            .git_bundle_read(&GitBundleReadParams {
                bundle_id: created.bundle_id.clone(),
                offset,
                limit: 1024,
            })
            .await
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&chunk.base64)
            .unwrap();
        assert_eq!(bytes.len() as u64, chunk.bytes_read);
        let mut chunk_hasher = Sha256::new();
        chunk_hasher.update(&bytes);
        assert_eq!(format!("{:x}", chunk_hasher.finalize()), chunk.chunk_sha256);
        hasher.update(&bytes);
        offset += chunk.bytes_read;
        frames += 1;
        if chunk.done {
            break;
        }
    }
    assert_eq!(frames, created.chunks);
    assert_eq!(offset, created.bytes);
    assert_eq!(format!("{:x}", hasher.finalize()), created.sha256);
    let deleted = client
        .git_bundle_delete(&GitBundleDeleteParams {
            bundle_id: created.bundle_id,
            operation_id: Some("stdio-delete".into()),
        })
        .await
        .unwrap();
    assert!(deleted.deleted);
}

#[tokio::test]
async fn packaged_helper_rejects_a_bundle_over_the_cap() {
    let dir = repo();
    let client = helper(dir.path()).await;
    let error = client
        .git_bundle_create(&GitBundleCreateParams {
            branch: "main".into(),
            base: None,
            max_bytes: 64,
            chunk_bytes: 1024,
            operation_id: Some("stdio-cap".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::FileTooLarge);
}

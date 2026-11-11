//! Bundle transport contract over a real helper server stream.
//!
//! The server is the same `serve` implementation the packaged helper uses; the
//! client is the same bounded JSON-RPC client. Only the transport is replaced
//! with an in-memory duplex, which makes chunking, capability negotiation, and
//! cleanup deterministic without SSH or a child process.

use std::fs;
use std::process::Command;
use std::time::Duration;

use base64::Engine as Base64Engine;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::duplex;
use vp_remote::protocol::*;
use vp_remote::{RemoteClient, RemoteClientConfig, ServerConfig, connect_stream, serve};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn git(root: &std::path::Path, args: &[&str]) {
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
    // A deterministic multi-kilobyte payload makes the bundle span several
    // 1 KiB chunks even with Git's compact object framing.
    let payload = (0..4096)
        .map(|index| format!("line-{index:04}-{}\n", "x".repeat(8)))
        .collect::<String>();
    fs::write(dir.path().join("payload.txt"), payload).unwrap();
    git(dir.path(), &["add", "a.txt", "payload.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    dir
}

async fn connect(root: &std::path::Path) -> RemoteClient {
    let config = ServerConfig::new(root, TOKEN).unwrap();
    let (client_stream, server_stream) = duplex(8 * 1024 * 1024);
    let (server_reader, server_writer) = tokio::io::split(server_stream);
    tokio::spawn(async move {
        let _ = serve(server_reader, server_writer, config).await;
    });
    let (client_reader, client_writer) = tokio::io::split(client_stream);
    connect_stream(
        client_reader,
        client_writer,
        HelloParams {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            client_version: "test".into(),
            auth_token: TOKEN.into(),
            root: root.canonicalize().unwrap().to_string_lossy().to_string(),
        },
        RemoteClientConfig {
            request_timeout: Duration::from_secs(20),
            handshake_timeout: Duration::from_secs(5),
            max_frame_bytes: MAX_FRAME_BYTES as usize,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn streams_a_bundle_in_bounded_chunks_and_deletes_it() {
    let dir = repo();
    let client = connect(dir.path()).await;
    assert!(client.supports_git_bundle());
    let created = client
        .git_bundle_create(&GitBundleCreateParams {
            branch: "main".into(),
            base: None,
            max_bytes: 4 * 1024 * 1024,
            chunk_bytes: 1024,
            operation_id: Some("bundle-create".into()),
        })
        .await
        .unwrap();
    assert_eq!(created.head.len(), 40);
    assert!(created.chunks > 1, "test bundle should span chunks");
    let mut offset = 0u64;
    let mut hasher = Sha256::new();
    let mut chunks = 0u32;
    loop {
        let chunk = client
            .git_bundle_read(&GitBundleReadParams {
                bundle_id: created.bundle_id.clone(),
                offset,
                limit: 1024,
            })
            .await
            .unwrap();
        assert!(chunk.bytes_read <= 1024);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&chunk.base64)
            .unwrap();
        let mut chunk_hasher = Sha256::new();
        chunk_hasher.update(&bytes);
        assert_eq!(format!("{:x}", chunk_hasher.finalize()), chunk.chunk_sha256);
        hasher.update(&bytes);
        offset += chunk.bytes_read;
        chunks += 1;
        if chunk.done {
            break;
        }
    }
    assert_eq!(offset, created.bytes);
    assert_eq!(chunks, created.chunks);
    assert_eq!(format!("{:x}", hasher.finalize()), created.sha256);
    let deleted = client
        .git_bundle_delete(&GitBundleDeleteParams {
            bundle_id: created.bundle_id.clone(),
            operation_id: Some("bundle-delete".into()),
        })
        .await
        .unwrap();
    assert!(deleted.deleted);
    let error = client
        .git_bundle_read(&GitBundleReadParams {
            bundle_id: created.bundle_id,
            offset: 0,
            limit: 1024,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::PathNotFound);
}

#[tokio::test]
async fn the_helper_enforces_the_byte_cap() {
    let dir = repo();
    let client = connect(dir.path()).await;
    let error = client
        .git_bundle_create(&GitBundleCreateParams {
            branch: "main".into(),
            base: None,
            max_bytes: 64,
            chunk_bytes: 1024,
            operation_id: Some("bundle-too-large".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::FileTooLarge);
}

#[tokio::test]
async fn read_and_delete_reject_unknown_bundle_ids_without_leaking_paths() {
    let dir = repo();
    let client = connect(dir.path()).await;
    let error = client
        .git_bundle_read(&GitBundleReadParams {
            bundle_id: "../etc/passwd".into(),
            offset: 0,
            limit: 1024,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::InvalidParams);
    let error = client
        .git_bundle_delete(&GitBundleDeleteParams {
            bundle_id: "missing".into(),
            operation_id: Some("delete-missing".into()),
        })
        .await
        .unwrap();
    assert!(!error.deleted);
}

//! End-to-end tests against the real `vp-remote-agent` binary over stdio.
//!
//! These tests intentionally use the packaged helper rather than a mock so the
//! handshake, framing, path jail, atomic writes, exec process groups, Git
//! worktrees, and ledger behaviour are exercised together.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine as Base64Engine;
use tempfile::TempDir;
use tokio::io::BufReader;
use vp_protocol::rpc::RpcResponse;
use vp_remote::framing::{read_frame, write_frame};
use vp_remote::protocol::*;
use vp_remote::{
    HelloParams, RemoteClient, RemoteClientConfig, RemoteErrorCode, RemoteTransport,
    StdioTransport, WriteEncoding, WriteFileParams,
};

const TOKEN: &str = "integration-token-0123456789abcdef";
const TOKEN_ENV: &str = "VERLINPARK_REMOTE_TEST_TOKEN";

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_vp-remote-agent"))
}

fn transport(root: &Path, ledger: Option<&Path>, extra: &[&str]) -> StdioTransport {
    let mut transport = StdioTransport::new(helper())
        .arg("remote-agent")
        .arg("--root")
        .arg(root.to_string_lossy().to_string())
        .arg("--token-env")
        .arg(TOKEN_ENV)
        .env(TOKEN_ENV, TOKEN);
    if let Some(ledger) = ledger {
        transport = transport
            .arg("--ledger")
            .arg(ledger.to_string_lossy().to_string());
    } else {
        transport = transport.arg("--no-ledger");
    }
    for argument in extra {
        transport = transport.arg(*argument);
    }
    transport
}

async fn connect(
    root: &Path,
    ledger: Option<&Path>,
    extra: &[&str],
) -> Result<RemoteClient, RemoteError> {
    connect_with_timeout(root, ledger, extra, Duration::from_secs(20)).await
}

async fn connect_with_timeout(
    root: &Path,
    ledger: Option<&Path>,
    extra: &[&str],
    request_timeout: Duration,
) -> Result<RemoteClient, RemoteError> {
    let root = root.canonicalize().unwrap();
    let hello = HelloParams {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        client_version: "integration-test".into(),
        auth_token: TOKEN.into(),
        root: root.to_string_lossy().to_string(),
    };
    RemoteClient::connect(
        &transport(&root, ledger, extra),
        hello,
        RemoteClientConfig {
            request_timeout,
            handshake_timeout: Duration::from_secs(10),
            max_frame_bytes: MAX_FRAME_BYTES as usize,
        },
    )
    .await
}

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

fn repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "first"]);
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

fn write_params(path: &str, content: &str, operation_id: &str) -> WriteFileParams {
    WriteFileParams {
        path: path.into(),
        content: content.into(),
        encoding: WriteEncoding::Utf8,
        expected_sha256: None,
        create_parents: true,
        allow_overwrite: false,
        mode: None,
        operation_id: Some(operation_id.into()),
    }
}

#[tokio::test]
async fn handshake_exposes_capabilities_and_probe() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let hello = client.hello();
    assert_eq!(hello.protocol_version, REMOTE_PROTOCOL_VERSION);
    assert!(hello.missing_required().is_empty());
    let probe = client.probe().await.unwrap();
    assert_eq!(
        probe.root,
        dir.path().canonicalize().unwrap().to_string_lossy()
    );
    assert!(probe.git_version.is_some());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn wrong_token_is_refused() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let hello = HelloParams {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        client_version: "test".into(),
        auth_token: "wrong-token-0123456789abcdef".into(),
        root: root.to_string_lossy().to_string(),
    };
    let error = RemoteClient::connect(
        &transport(&root, None, &[]),
        hello,
        RemoteClientConfig::default(),
    )
    .await
    .err()
    .expect("wrong token must fail");
    assert_eq!(error.code, RemoteErrorCode::Unauthorized);
}

#[tokio::test]
async fn wrong_protocol_version_is_refused() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let hello = HelloParams {
        protocol_version: REMOTE_PROTOCOL_VERSION + 1,
        client_version: "test".into(),
        auth_token: TOKEN.into(),
        root: root.to_string_lossy().to_string(),
    };
    let error = RemoteClient::connect(
        &transport(&root, None, &[]),
        hello,
        RemoteClientConfig::default(),
    )
    .await
    .err()
    .expect("version mismatch must fail");
    assert_eq!(error.code, RemoteErrorCode::ProtocolVersionMismatch);
}

#[tokio::test]
async fn operations_before_hello_are_refused() {
    let dir = TempDir::new().unwrap();
    let mut process = transport(dir.path(), None, &[]).spawn().await.unwrap();
    let mut stdin = process.take_stdin().unwrap();
    let stdout = process.take_stdout().unwrap();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 9,
        "method": "stat",
        "params": {"path": "."}
    });
    write_frame(&mut stdin, &request, MAX_FRAME_BYTES as usize)
        .await
        .unwrap();
    let mut reader = BufReader::new(stdout);
    let frame = read_frame(&mut reader, MAX_FRAME_BYTES as usize)
        .await
        .unwrap()
        .unwrap();
    let response: RpcResponse = serde_json::from_slice(&frame).unwrap();
    let error = response.error.expect("pre-hello operation must fail");
    let remote: vp_remote::RemoteError = serde_json::from_value(error.data.unwrap()).unwrap();
    assert_eq!(remote.code, RemoteErrorCode::Unauthorized);
    process.shutdown(Duration::from_secs(2)).await;
}

#[tokio::test]
async fn atomic_write_sha_conflict_and_idempotency() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let first = client
        .write_file(&write_params("src/a.txt", "one", "write-1"))
        .await
        .unwrap();
    assert!(first.created);
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.txt")).unwrap(),
        "one"
    );

    let replay = client
        .write_file(&write_params("src/a.txt", "one", "write-1"))
        .await
        .unwrap();
    assert!(replay.replayed);

    let conflict = client
        .write_file(&write_params("src/a.txt", "different", "write-1"))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, RemoteErrorCode::OperationIdConflict);

    let stale = client
        .write_file(&write_params("src/a.txt", "two", "write-2"))
        .await
        .unwrap_err();
    assert_eq!(stale.code, RemoteErrorCode::PreconditionRequired);

    let mut wrong = write_params("src/a.txt", "two", "write-3");
    wrong.expected_sha256 = Some("0".repeat(64));
    assert_eq!(
        client.write_file(&wrong).await.unwrap_err().code,
        RemoteErrorCode::ShaMismatch
    );

    let mut correct = write_params("src/a.txt", "two", "write-4");
    correct.expected_sha256 = Some(first.sha256.clone());
    let replaced = client.write_file(&correct).await.unwrap();
    assert!(replaced.replaced);
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.txt")).unwrap(),
        "two"
    );
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn path_traversal_and_symlink_escape_are_refused() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret"), "no").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    assert_eq!(
        client
            .read_file(&ReadFileParams {
                path: "../outside".into(),
                offset: 0,
                limit: 100,
                encoding: ReadEncoding::Auto,
            })
            .await
            .unwrap_err()
            .code,
        RemoteErrorCode::PathEscape
    );
    let escape = client
        .read_file(&ReadFileParams {
            path: "escape/secret".into(),
            offset: 0,
            limit: 100,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap_err();
    assert_eq!(escape.code, RemoteErrorCode::PathEscape);
    let write = client
        .write_file(&write_params("escape/new", "x", "escape-write"))
        .await
        .unwrap_err();
    assert!(matches!(
        write.code,
        RemoteErrorCode::PermissionDenied | RemoteErrorCode::PathEscape
    ));
    assert!(!outside.path().join("new").exists());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn reads_support_offsets_and_base64() {
    let dir = TempDir::new().unwrap();
    let bytes = [0u8, 159, 146, 150, b'h', b'i'];
    fs::write(dir.path().join("bin"), bytes).unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let binary = client
        .read_file(&ReadFileParams {
            path: "bin".into(),
            offset: 0,
            limit: 4,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap();
    assert_eq!(binary.encoding, ReadEncoding::Base64);
    assert_eq!(
        binary.base64.as_deref(),
        Some(
            base64::engine::general_purpose::STANDARD
                .encode([0u8, 159, 146, 150])
                .as_str()
        )
    );
    assert!(binary.truncated);
    let text = client
        .read_file(&ReadFileParams {
            path: "bin".into(),
            offset: 4,
            limit: 2,
            encoding: ReadEncoding::Utf8,
        })
        .await
        .unwrap();
    assert_eq!(text.text.as_deref(), Some("hi"));
    assert!(!text.truncated);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn exec_caps_output_and_honours_the_environment_allowlist() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &["--env-allow", "CUSTOM"])
        .await
        .unwrap();
    let mut env = BTreeMap::new();
    env.insert("CUSTOM".to_string(), "yes".to_string());
    let result = client
        .exec(&ExecParams {
            command: "printf $CUSTOM; printf err >&2".into(),
            cwd: None,
            env,
            timeout_ms: 5_000,
            output_cap_bytes: 64 * 1024,
            operation_id: Some("exec-1".into()),
        })
        .await
        .unwrap();
    assert_eq!(result.stdout, "yes");
    assert_eq!(result.stderr, "err");
    let denied = client
        .exec(&ExecParams {
            command: "true".into(),
            cwd: None,
            env: BTreeMap::from([("OPENAI_API_KEY".to_string(), "secret".to_string())]),
            timeout_ms: 5_000,
            output_cap_bytes: 64 * 1024,
            operation_id: Some("exec-2".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(denied.code, RemoteErrorCode::EnvNotAllowed);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn exec_timeout_and_client_cancellation_recover() {
    let dir = TempDir::new().unwrap();
    let client = connect_with_timeout(dir.path(), None, &[], Duration::from_millis(250))
        .await
        .unwrap();
    let timed = client
        .exec(&ExecParams {
            command: "sleep 30 & wait".into(),
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: 150,
            output_cap_bytes: 64 * 1024,
            operation_id: Some("exec-timeout".into()),
        })
        .await
        .unwrap();
    assert!(timed.timed_out);
    assert!(timed.exit_code.is_none());

    let cancelled = client
        .exec(&ExecParams {
            command: "sleep 30 & wait".into(),
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: 30_000,
            output_cap_bytes: 64 * 1024,
            operation_id: Some("exec-cancel".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(cancelled.code, RemoteErrorCode::Timeout);
    // The connection is still usable after cancellation.
    let probe = client.probe().await.unwrap();
    assert_eq!(probe.protocol_version, REMOTE_PROTOCOL_VERSION);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn git_worktree_lifecycle_is_remote_and_rooted() {
    let (_dir, root) = repo();
    let client = connect(&root, None, &[]).await.unwrap();
    let status = client.git_status().await.unwrap();
    assert!(status.clean);
    assert!(status.head.is_some());
    let added = client
        .git_worktree_add(&GitWorktreeAddParams {
            path: "worktrees/task".into(),
            branch: "verlinpark/task".into(),
            start_from: Some("HEAD".into()),
            create_branch: true,
            operation_id: Some("worktree-add".into()),
        })
        .await
        .unwrap();
    assert!(Path::new(&added.entry.path).starts_with(&root));
    let list = client.git_worktree_list().await.unwrap();
    assert!(
        list.entries
            .iter()
            .any(|entry| entry.path == added.entry.path)
    );
    let removed = client
        .git_worktree_remove(&GitWorktreeRemoveParams {
            path: "worktrees/task".into(),
            force: true,
            operation_id: Some("worktree-remove".into()),
        })
        .await
        .unwrap();
    assert!(removed.removed);
    let replay = client
        .git_worktree_remove(&GitWorktreeRemoveParams {
            path: "worktrees/task".into(),
            force: true,
            operation_id: Some("worktree-remove".into()),
        })
        .await
        .unwrap();
    assert!(replay.replayed);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn persisted_ledger_replays_across_helper_restart() {
    let dir = TempDir::new().unwrap();
    let ledger = dir.path().join("ledger.jsonl");
    {
        let client = connect(dir.path(), Some(&ledger), &[]).await.unwrap();
        let result = client
            .write_file(&write_params("a.txt", "one", "restart-write"))
            .await
            .unwrap();
        assert!(result.created);
        client.shutdown().await.unwrap();
    }
    {
        let client = connect(dir.path(), Some(&ledger), &[]).await.unwrap();
        let replay = client
            .write_file(&write_params("a.txt", "one", "restart-write"))
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one");
        client.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn mkdir_remove_and_rename_are_audited() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let created = client
        .mkdir(&MkdirParams {
            path: "a/b".into(),
            recursive: true,
            operation_id: Some("mkdir-1".into()),
        })
        .await
        .unwrap();
    assert!(created.created);
    client
        .write_file(&write_params("a/b/one.txt", "one", "write-a"))
        .await
        .unwrap();
    let renamed = client
        .rename(&RenameParams {
            from: "a/b/one.txt".into(),
            to: "a/two.txt".into(),
            overwrite: false,
            create_parents: false,
            operation_id: Some("rename-1".into()),
        })
        .await
        .unwrap();
    assert_eq!(renamed.to, "a/two.txt");
    let removed = client
        .remove(&RemoveParams {
            path: "a".into(),
            recursive: true,
            missing_ok: false,
            operation_id: Some("remove-1".into()),
        })
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!dir.path().join("a").exists());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_dir_paginates_through_the_helper() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("dir")).unwrap();
    fs::write(dir.path().join("a"), "a").unwrap();
    fs::write(dir.path().join(".hidden"), "h").unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let first = client
        .list_dir(&ListDirParams {
            path: ".".into(),
            include_hidden: false,
            limit: 1,
            cursor: 0,
        })
        .await
        .unwrap();
    assert_eq!(first.total, 2);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.next_cursor, Some(1));
    let second = client
        .list_dir(&ListDirParams {
            path: ".".into(),
            include_hidden: false,
            limit: 10,
            cursor: 1,
        })
        .await
        .unwrap();
    assert_eq!(second.entries.len(), 1);
    assert!(second.next_cursor.is_none());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_requests_are_multiplexed_by_id() {
    let dir = TempDir::new().unwrap();
    for index in 0..10 {
        fs::write(dir.path().join(format!("file-{index}.txt")), "x").unwrap();
    }
    let client = std::sync::Arc::new(connect(dir.path(), None, &[]).await.unwrap());
    let mut tasks = Vec::new();
    for index in 0..10u64 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            client
                .read_file(&ReadFileParams {
                    path: format!("file-{index}.txt"),
                    offset: 0,
                    limit: 1,
                    encoding: ReadEncoding::Auto,
                })
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        let result = task.await.unwrap();
        assert_eq!(result.text.as_deref(), Some("x"));
    }
    let client = std::sync::Arc::into_inner(client).expect("sole client owner");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn exec_replay_does_not_repeat_the_side_effect() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let params = ExecParams {
        command: "printf x >> counter; wc -c < counter".into(),
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: 5_000,
        output_cap_bytes: 64 * 1024,
        operation_id: Some("exec-once".into()),
    };
    let first = client.exec(&params).await.unwrap();
    assert!(first.stdout.trim().starts_with('1'));
    assert!(!first.replayed);
    let second = client.exec(&params).await.unwrap();
    assert!(second.replayed);
    assert_eq!(second.stdout, first.stdout);
    assert_eq!(fs::read(dir.path().join("counter")).unwrap(), b"x");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn base64_writes_and_stat_round_trip() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let bytes = [0u8, 1, 2, 159, 160, 255];
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut params = write_params("bin/data", &encoded, "binary-write");
    params.encoding = WriteEncoding::Base64;
    params.mode = Some(0o600);
    let result = client.write_file(&params).await.unwrap();
    assert_eq!(result.size, bytes.len() as u64);
    assert_eq!(fs::read(dir.path().join("bin/data")).unwrap(), bytes);
    let stat = client
        .stat(&StatParams {
            path: "bin/data".into(),
        })
        .await
        .unwrap();
    assert_eq!(stat.kind, vp_remote::EntryKind::File);
    assert_eq!(stat.size, bytes.len() as u64);
    assert_eq!(stat.mode & 0o777, 0o600);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn mkdir_rename_and_remove_conflicts_are_explicit() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let first = client
        .mkdir(&MkdirParams {
            path: "d".into(),
            recursive: false,
            operation_id: Some("mkdir-d".into()),
        })
        .await
        .unwrap();
    assert!(first.created);
    let again = client
        .mkdir(&MkdirParams {
            path: "d".into(),
            recursive: false,
            operation_id: Some("mkdir-d-2".into()),
        })
        .await
        .unwrap();
    assert!(!again.created);
    client
        .write_file(&write_params("d/a", "a", "write-d-a"))
        .await
        .unwrap();
    client
        .write_file(&write_params("d/b", "b", "write-d-b"))
        .await
        .unwrap();
    let conflict = client
        .rename(&RenameParams {
            from: "d/a".into(),
            to: "d/b".into(),
            overwrite: false,
            create_parents: false,
            operation_id: Some("rename-conflict".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(conflict.code, RemoteErrorCode::DestinationExists);
    let overwritten = client
        .rename(&RenameParams {
            from: "d/a".into(),
            to: "d/b".into(),
            overwrite: true,
            create_parents: false,
            operation_id: Some("rename-overwrite".into()),
        })
        .await
        .unwrap();
    assert_eq!(overwritten.to, "d/b");
    assert_eq!(fs::read_to_string(dir.path().join("d/b")).unwrap(), "a");
    let missing = client
        .remove(&RemoveParams {
            path: "d/missing".into(),
            recursive: false,
            missing_ok: true,
            operation_id: Some("remove-missing".into()),
        })
        .await
        .unwrap();
    assert!(!missing.removed);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn git_diff_log_and_rev_parse_are_structured() {
    let (_dir, root) = repo();
    fs::write(root.join("a.txt"), "changed\n").unwrap();
    let client = connect(&root, None, &[]).await.unwrap();
    let diff = client
        .git_diff(&GitDiffParams {
            scope: DiffScope::Unstaged,
            base: None,
            commit: None,
            paths: vec!["a.txt".into()],
            context: 2,
            stat_only: false,
        })
        .await
        .unwrap();
    assert!(diff.patch.contains("changed"));
    assert!(diff.stat.contains("a.txt"));
    let log = client
        .git_log(&GitLogParams {
            limit: 5,
            paths: Vec::new(),
            since: None,
        })
        .await
        .unwrap();
    assert_eq!(log.entries.len(), 1);
    assert_eq!(log.entries[0].subject, "first");
    let head = client
        .git_rev_parse(&GitRevParseParams {
            rev: "HEAD".into(),
            commit: true,
        })
        .await
        .unwrap();
    assert_eq!(head.value.len(), 40);
    let missing = client
        .git_rev_parse(&GitRevParseParams {
            rev: "does-not-exist".into(),
            commit: true,
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, RemoteErrorCode::PathNotFound);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn symlinks_inside_the_root_are_readable_but_not_escapes() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("target"), "inside").unwrap();
    std::os::unix::fs::symlink("target", dir.path().join("link")).unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let read = client
        .read_file(&ReadFileParams {
            path: "link".into(),
            offset: 0,
            limit: 10,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap();
    assert_eq!(read.text.as_deref(), Some("inside"));
    let remove = client
        .remove(&RemoveParams {
            path: "link".into(),
            recursive: false,
            missing_ok: false,
            operation_id: Some("remove-link".into()),
        })
        .await
        .unwrap();
    assert!(remove.removed);
    assert!(dir.path().join("target").exists());
    assert!(!dir.path().join("link").exists());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn ledger_status_exposes_a_bounded_audit_tail() {
    let dir = TempDir::new().unwrap();
    let ledger = dir.path().join("ops.jsonl");
    let client = connect(dir.path(), Some(&ledger), &[]).await.unwrap();
    for index in 0..3 {
        client
            .write_file(&write_params(
                &format!("f-{index}"),
                "x",
                &format!("ledger-write-{index}"),
            ))
            .await
            .unwrap();
    }
    let stats = client
        .ledger_stats(&vp_remote::LedgerStatsParams { limit: 2 })
        .await
        .unwrap();
    assert!(stats.persisted);
    assert_eq!(stats.entries, 3);
    assert_eq!(stats.tail.len(), 2);
    assert_eq!(stats.tail[1].operation_id, "ledger-write-2");
    assert!(stats.tail.iter().all(|entry| entry.success));
    assert!(ledger.is_file());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn stat_reports_symlink_metadata_without_following_escapes() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("target"), "inside").unwrap();
    std::os::unix::fs::symlink("target", dir.path().join("link")).unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let stat = client
        .stat(&StatParams {
            path: "link".into(),
        })
        .await
        .unwrap();
    assert_eq!(stat.kind, vp_remote::EntryKind::Symlink);
    assert_eq!(stat.symlink_target.as_deref(), Some("target"));
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
    let escaped = client
        .stat(&StatParams {
            path: "escape".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(escaped.code, RemoteErrorCode::PathEscape);
    client.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn ledger_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let ledger = dir.path().join("nested").join("ops.jsonl");
    let client = connect(dir.path(), Some(&ledger), &[]).await.unwrap();
    client
        .write_file(&write_params("a", "a", "private-ledger-write"))
        .await
        .unwrap();
    let mode = fs::metadata(&ledger).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let directory_mode = fs::metadata(ledger.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_reports_symlinks_and_filters_git_metadata() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    fs::write(dir.path().join(".git/config"), "x").unwrap();
    fs::write(dir.path().join("target"), "x").unwrap();
    std::os::unix::fs::symlink("target", dir.path().join("link")).unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let listing = client
        .list_dir(&ListDirParams {
            path: ".".into(),
            include_hidden: true,
            limit: 100,
            cursor: 0,
        })
        .await
        .unwrap();
    assert!(listing.entries.iter().any(|entry| entry.name == ".git"));
    let link = listing
        .entries
        .iter()
        .find(|entry| entry.name == "link")
        .unwrap();
    assert_eq!(link.kind, vp_remote::EntryKind::Symlink);
    assert_eq!(link.symlink_target.as_deref(), Some("target"));
    let git = client
        .read_file(&ReadFileParams {
            path: ".git/config".into(),
            offset: 0,
            limit: 10,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap_err();
    assert_eq!(git.code, RemoteErrorCode::PermissionDenied);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_errors_are_specific() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("directory")).unwrap();
    fs::write(dir.path().join("file"), "short").unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let directory = client
        .read_file(&ReadFileParams {
            path: "directory".into(),
            offset: 0,
            limit: 10,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap_err();
    assert_eq!(directory.code, RemoteErrorCode::NotAFile);
    let offset = client
        .read_file(&ReadFileParams {
            path: "file".into(),
            offset: 100,
            limit: 10,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap_err();
    assert_eq!(offset.code, RemoteErrorCode::InvalidParams);
    let missing = client
        .read_file(&ReadFileParams {
            path: "missing".into(),
            offset: 0,
            limit: 10,
            encoding: ReadEncoding::Auto,
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, RemoteErrorCode::PathNotFound);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn write_requires_parents_to_exist_unless_requested() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let mut params = write_params("missing/child/file", "x", "no-parents");
    params.create_parents = false;
    let error = client.write_file(&params).await.unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::PathNotFound);
    let mut params = write_params("missing/child/file", "x", "with-parents");
    params.create_parents = true;
    let result = client.write_file(&params).await.unwrap();
    assert!(result.created);
    assert_eq!(
        fs::read_to_string(dir.path().join("missing/child/file")).unwrap(),
        "x"
    );
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn remove_requires_recursive_for_nonempty_directories() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("d/sub")).unwrap();
    fs::write(dir.path().join("d/sub/file"), "x").unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let error = client
        .remove(&RemoveParams {
            path: "d".into(),
            recursive: false,
            missing_ok: false,
            operation_id: Some("remove-nonrecursive".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::DirectoryNotEmpty);
    let removed = client
        .remove(&RemoveParams {
            path: "d".into(),
            recursive: true,
            missing_ok: false,
            operation_id: Some("remove-recursive".into()),
        })
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!dir.path().join("d").exists());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn exec_cwd_is_jailed_and_output_caps_are_enforced() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let cwd = client
        .exec(&ExecParams {
            command: "pwd".into(),
            cwd: Some("sub".into()),
            env: BTreeMap::new(),
            timeout_ms: 5_000,
            output_cap_bytes: 64 * 1024,
            operation_id: Some("exec-cwd".into()),
        })
        .await
        .unwrap();
    assert!(cwd.stdout.trim().ends_with("/sub"));
    let escape = client
        .exec(&ExecParams {
            command: "pwd".into(),
            cwd: Some("../outside".into()),
            env: BTreeMap::new(),
            timeout_ms: 5_000,
            output_cap_bytes: 64 * 1024,
            operation_id: Some("exec-cwd-escape".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(escape.code, RemoteErrorCode::PathEscape);
    let capped = client
        .exec(&ExecParams {
            command: "yes x | head -c 200000".into(),
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: 5_000,
            output_cap_bytes: 1024,
            operation_id: Some("exec-capped".into()),
        })
        .await
        .unwrap();
    assert!(capped.stdout_truncated);
    assert!(capped.stdout.len() <= 1024);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn mutation_without_an_operation_id_is_refused() {
    let dir = TempDir::new().unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let mut params = write_params("a", "a", "placeholder");
    params.operation_id = None;
    let error = client.write_file(&params).await.unwrap_err();
    assert_eq!(error.code, RemoteErrorCode::OperationIdRequired);
    assert!(!dir.path().join("a").exists());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn list_cursor_past_the_end_returns_an_empty_page() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a"), "a").unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let page = client
        .list_dir(&ListDirParams {
            path: ".".into(),
            include_hidden: true,
            limit: 10,
            cursor: 100,
        })
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert!(page.entries.is_empty());
    assert!(page.next_cursor.is_none());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_overwrite_replaces_without_a_sha() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a"), "old").unwrap();
    let client = connect(dir.path(), None, &[]).await.unwrap();
    let mut params = write_params("a", "new", "overwrite");
    params.allow_overwrite = true;
    let result = client.write_file(&params).await.unwrap();
    assert!(result.replaced);
    assert!(!result.created);
    assert_eq!(fs::read_to_string(dir.path().join("a")).unwrap(), "new");
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn worktree_add_can_attach_an_existing_branch() {
    let (_dir, root) = repo();
    let client = connect(&root, None, &[]).await.unwrap();
    client
        .git_worktree_add(&GitWorktreeAddParams {
            path: "worktrees/one".into(),
            branch: "existing-branch".into(),
            start_from: Some("HEAD".into()),
            create_branch: true,
            operation_id: Some("create-branch".into()),
        })
        .await
        .unwrap();
    client
        .git_worktree_remove(&GitWorktreeRemoveParams {
            path: "worktrees/one".into(),
            force: true,
            operation_id: Some("remove-one".into()),
        })
        .await
        .unwrap();
    let attached = client
        .git_worktree_add(&GitWorktreeAddParams {
            path: "worktrees/two".into(),
            branch: "existing-branch".into(),
            start_from: None,
            create_branch: false,
            operation_id: Some("attach-branch".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        attached.entry.branch.as_deref(),
        Some("refs/heads/existing-branch")
    );
    client.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn token_file_authentication_requires_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let token_path = dir.path().join("token");
    fs::write(&token_path, TOKEN).unwrap();
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).unwrap();
    let transport = StdioTransport::new(helper())
        .arg("remote-agent")
        .arg("--root")
        .arg(root.to_string_lossy().to_string())
        .arg("--token-file")
        .arg(token_path.to_string_lossy().to_string())
        .arg("--no-ledger");
    let hello = HelloParams {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        client_version: "test".into(),
        auth_token: TOKEN.into(),
        root: root.to_string_lossy().to_string(),
    };
    let client = RemoteClient::connect(&transport, hello.clone(), RemoteClientConfig::default())
        .await
        .unwrap();
    assert_eq!(client.probe().await.unwrap().root, root.to_string_lossy());
    client.shutdown().await.unwrap();

    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
    let error = RemoteClient::connect(&transport, hello, RemoteClientConfig::default())
        .await
        .err()
        .expect("insecure token file must be refused");
    assert_eq!(error.code, RemoteErrorCode::TransportClosed);
}

//! Public wire-contract tests for the remote protocol.
//!
//! These tests exercise the serde contract independently of the helper binary:
//! old clients that omit new fields must still decode, bounded defaults must be
//! applied, and every operation envelope must survive a JSON round trip.

use serde_json::json;
use vp_remote::protocol::*;

#[test]
fn execution_envelopes_round_trip() {
    let exec = ExecParams {
        command: "make test".into(),
        cwd: Some("sub".into()),
        env: [("CI".to_string(), "1".to_string())].into(),
        timeout_ms: 1_000,
        output_cap_bytes: 2_048,
        operation_id: Some("exec-1".into()),
    };
    let value = serde_json::to_value(&exec).unwrap();
    let decoded: ExecParams = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, exec);
    let result = ExecResult {
        exit_code: Some(0),
        stdout: "ok".into(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        timed_out: false,
        cancelled: false,
        duration_ms: 2,
        operation_id: Some("exec-1".into()),
        replayed: false,
    };
    let decoded: ExecResult =
        serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
    assert_eq!(decoded, result);
}

#[test]
fn legacy_exec_params_get_bounded_defaults() {
    let params: ExecParams = serde_json::from_value(json!({
        "command": "true",
        "operation_id": "legacy"
    }))
    .unwrap();
    assert_eq!(params.timeout_ms, DEFAULT_EXEC_TIMEOUT_MS);
    assert_eq!(params.output_cap_bytes, DEFAULT_EXEC_OUTPUT_BYTES);
    assert!(params.env.is_empty());
    assert!(params.cwd.is_none());
}

#[test]
fn file_operation_defaults_are_safe() {
    let write: WriteFileParams = serde_json::from_value(json!({
        "path": "a",
        "content": "b"
    }))
    .unwrap();
    assert!(!write.allow_overwrite);
    assert!(write.create_parents);
    assert!(write.expected_sha256.is_none());

    let list: ListDirParams = serde_json::from_value(json!({})).unwrap();
    assert_eq!(list.path, ".");
    assert!(list.include_hidden);
    assert_eq!(list.limit, DEFAULT_DIR_ENTRIES);

    let read: ReadFileParams = serde_json::from_value(json!({"path": "a"})).unwrap();
    assert_eq!(read.limit, DEFAULT_READ_BYTES);
    assert_eq!(read.encoding, ReadEncoding::Auto);
}

#[test]
fn git_operation_envelopes_round_trip() {
    let diff = GitDiffParams {
        scope: DiffScope::Base,
        base: Some("main".into()),
        commit: None,
        paths: vec!["src".into()],
        context: 2,
        stat_only: true,
    };
    let decoded: GitDiffParams =
        serde_json::from_value(serde_json::to_value(&diff).unwrap()).unwrap();
    assert_eq!(decoded, diff);

    let status = GitStatusResult {
        branch: Some("main".into()),
        head: Some("abc".into()),
        upstream: None,
        ahead: 1,
        behind: 2,
        clean: false,
        entries: vec![GitStatusEntry {
            status: " M".into(),
            path: "a".into(),
            original_path: None,
        }],
    };
    let decoded: GitStatusResult =
        serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
    assert_eq!(decoded, status);

    let worktree = GitWorktreeAddParams {
        path: "worktrees/t".into(),
        branch: "feature/t".into(),
        start_from: Some("HEAD".into()),
        create_branch: true,
        operation_id: Some("wt-1".into()),
    };
    let decoded: GitWorktreeAddParams =
        serde_json::from_value(serde_json::to_value(&worktree).unwrap()).unwrap();
    assert_eq!(decoded, worktree);
}

#[test]
fn bundle_envelopes_round_trip_and_default_bounds() {
    let create = GitBundleCreateParams {
        branch: "feature/remote".into(),
        base: None,
        max_bytes: 1024 * 1024,
        chunk_bytes: 4096,
        operation_id: Some("bundle-1".into()),
    };
    let decoded: GitBundleCreateParams =
        serde_json::from_value(serde_json::to_value(&create).unwrap()).unwrap();
    assert_eq!(decoded, create);
    let created = GitBundleCreateResult {
        bundle_id: "b1".into(),
        branch: "feature/remote".into(),
        head: "a".repeat(40),
        export_ref: "refs/verlinpark/export/b1".into(),
        base: Some("c".repeat(40)),
        bytes: 10_000,
        sha256: "b".repeat(64),
        chunk_bytes: 4096,
        chunks: 3,
        expires_at_unix_ms: 1_700_000_000_000,
        operation_id: Some("bundle-1".into()),
        replayed: false,
    };
    let decoded: GitBundleCreateResult =
        serde_json::from_value(serde_json::to_value(&created).unwrap()).unwrap();
    assert_eq!(decoded, created);
    let legacy: GitBundleCreateParams = serde_json::from_value(json!({
        "branch": "main",
        "max_bytes": 1024
    }))
    .unwrap();
    assert_eq!(legacy.chunk_bytes, DEFAULT_GIT_BUNDLE_CHUNK_BYTES);
    assert!(legacy.operation_id.is_none());
}

#[test]
fn hello_round_trips_and_reports_missing_capabilities() {
    let hello = HelloParams {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        client_version: "test".into(),
        auth_token: "token-token-token".into(),
        root: "/srv/project".into(),
    };
    let decoded: HelloParams =
        serde_json::from_value(serde_json::to_value(&hello).unwrap()).unwrap();
    assert_eq!(decoded, hello);
    let result = HelloResult {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        helper_version: "1".into(),
        root: "/srv/project".into(),
        capabilities: REQUIRED_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        env_allow: vec!["CI".into()],
        server_started_at: "now".into(),
    };
    assert!(result.missing_required().is_empty());
    assert!(result.has_capability(CAP_EXEC));
    assert!(!result.has_capability("not-a-capability"));
}

#[test]
fn ledger_status_round_trips() {
    let stats = LedgerStatsResult {
        entries: 4,
        max_entries: 512,
        persisted: true,
        file_bytes: 1024,
        tail: vec![LedgerEntrySummary {
            operation_id: "op".into(),
            method: METHOD_WRITE_FILE.into(),
            success: true,
            completed_at: "now".into(),
        }],
    };
    let decoded: LedgerStatsResult =
        serde_json::from_value(serde_json::to_value(&stats).unwrap()).unwrap();
    assert_eq!(decoded, stats);
    let params: LedgerStatsParams = serde_json::from_value(json!({})).unwrap();
    assert_eq!(params.limit, 20);
}

#[test]
fn remote_errors_carry_actionable_stable_codes() {
    let error = RemoteError::new(RemoteErrorCode::ShaMismatch, "file changed")
        .with_hint("read it again")
        .with_operation("op-1");
    let rpc = error.to_rpc_error();
    assert_eq!(rpc.code, -32010);
    let data = rpc.data.unwrap();
    assert_eq!(data["code"], "SHA_MISMATCH");
    assert_eq!(data["retryable"], false);
    assert_eq!(data["hint"], "read it again");
    assert_eq!(data["operation_id"], "op-1");
    let decoded: RemoteError = serde_json::from_value(data).unwrap();
    assert_eq!(decoded, error);
}

#[test]
fn error_codes_map_to_jsonrpc_and_retry_semantics() {
    assert_eq!(RemoteErrorCode::Unauthorized.json_rpc_code(), -32001);
    assert_eq!(
        RemoteErrorCode::UnsupportedOperation.json_rpc_code(),
        -32601
    );
    assert_eq!(RemoteErrorCode::PathNotFound.json_rpc_code(), -32004);
    assert!(RemoteErrorCode::Timeout.retryable());
    assert!(RemoteErrorCode::TransportClosed.retryable());
    assert!(!RemoteErrorCode::PathEscape.retryable());
    assert!(!RemoteErrorCode::ShaMismatch.retryable());
}

#[test]
fn cancel_and_shutdown_envelopes_round_trip() {
    let cancel = CancelParams {
        id: json!(7),
        reason: Some("timeout".into()),
    };
    let decoded: CancelParams =
        serde_json::from_value(serde_json::to_value(&cancel).unwrap()).unwrap();
    assert_eq!(decoded, cancel);
    let shutdown = ShutdownParams {
        reason: Some("done".into()),
    };
    let decoded: ShutdownParams =
        serde_json::from_value(serde_json::to_value(&shutdown).unwrap()).unwrap();
    assert_eq!(decoded, shutdown);
}

#[test]
fn path_and_entry_kinds_serialize_snake_case() {
    assert_eq!(serde_json::to_value(EntryKind::File).unwrap(), "file");
    assert_eq!(
        serde_json::to_value(EntryKind::Directory).unwrap(),
        "directory"
    );
    assert_eq!(
        serde_json::to_value(ReadEncoding::Base64).unwrap(),
        "base64"
    );
    assert_eq!(serde_json::to_value(WriteEncoding::Utf8).unwrap(), "utf8");
    assert_eq!(
        serde_json::to_value(DiffScope::Unstaged).unwrap(),
        "unstaged"
    );
}

#[test]
fn file_results_round_trip_with_optional_text_or_base64() {
    let stat = StatResult {
        path: "a".into(),
        kind: EntryKind::Symlink,
        size: 3,
        mode: 0o777,
        modified_unix_ms: Some(123),
        symlink_target: Some("target".into()),
    };
    let decoded: StatResult = serde_json::from_value(serde_json::to_value(&stat).unwrap()).unwrap();
    assert_eq!(decoded, stat);
    let listing = ListDirResult {
        path: ".".into(),
        entries: vec![DirEntry {
            name: "a".into(),
            path: "a".into(),
            kind: EntryKind::File,
            size: 1,
            mode: 0o644,
            symlink_target: None,
        }],
        total: 1,
        next_cursor: None,
        truncated: false,
    };
    let decoded: ListDirResult =
        serde_json::from_value(serde_json::to_value(&listing).unwrap()).unwrap();
    assert_eq!(decoded, listing);
    let read = ReadFileResult {
        path: "a".into(),
        sha256: "0".repeat(64),
        size: 1,
        offset: 0,
        bytes_read: 1,
        encoding: ReadEncoding::Base64,
        text: None,
        base64: Some("YQ==".into()),
        truncated: false,
    };
    let decoded: ReadFileResult =
        serde_json::from_value(serde_json::to_value(&read).unwrap()).unwrap();
    assert_eq!(decoded, read);
    let write = WriteFileResult {
        path: "a".into(),
        sha256: "0".repeat(64),
        size: 1,
        created: false,
        replaced: true,
        operation_id: Some("op".into()),
        replayed: true,
    };
    let decoded: WriteFileResult =
        serde_json::from_value(serde_json::to_value(&write).unwrap()).unwrap();
    assert_eq!(decoded, write);
}

#[test]
fn probe_and_worktree_results_round_trip() {
    let probe = ProbeResult {
        protocol_version: REMOTE_PROTOCOL_VERSION,
        helper_version: "1".into(),
        root: "/srv".into(),
        pid: 1,
        os: "linux".into(),
        arch: "aarch64".into(),
        uptime_secs: 2,
        git_version: Some("git version 2".into()),
        capabilities: vec![CAP_GIT.to_string()],
    };
    let decoded: ProbeResult =
        serde_json::from_value(serde_json::to_value(&probe).unwrap()).unwrap();
    assert_eq!(decoded, probe);
    let worktree = WorktreeEntry {
        path: "/srv/worktrees/a".into(),
        head: Some("abc".into()),
        branch: Some("refs/heads/a".into()),
        bare: false,
        detached: false,
        locked: None,
        prunable: None,
    };
    let decoded: WorktreeEntry =
        serde_json::from_value(serde_json::to_value(&worktree).unwrap()).unwrap();
    assert_eq!(decoded, worktree);
}

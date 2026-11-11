//! Public contract tests for Q3 tool errors, recipes, and the activity
//! projection. Unit tests cover internals; this file verifies the surface other
//! crates and clients consume.

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::tooling::errors::{
    ToolErrorCode, ToolFailure, catalog, classify_message, failure_from_anyhow,
};
use vp_core::tooling::recipes;
use vp_core::tools::{ToolContext, ToolOutput, ToolRegistry};
use vp_store::Store;

fn git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("README.md"), "hello\nworld\n").unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.test"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    dir
}

fn context(root: &Path) -> ToolContext {
    ToolContext {
        root: root.to_path_buf(),
        project_root: root.to_path_buf(),
        sandbox_root: root.join(".sandbox"),
        home: root.join("home"),
        restrict_home: false,
        read_jail: false,
        git_branch: Some("main".to_string()),
        secret_roots: Vec::new(),
        network_allowed: false,
        remote: None,
        session_id: None,
        process_supervisor: None,
        test_cache: None,
    }
}

fn code_of(output: &ToolOutput) -> String {
    output.structured.as_ref().unwrap()["code"]
        .as_str()
        .unwrap_or("missing")
        .to_string()
}

#[tokio::test]
async fn registry_failures_use_the_public_stable_contract() {
    let dir = repo();
    let context = context(dir.path());
    let registry = ToolRegistry::default_tools();

    let read = registry.get("read_file").unwrap();
    let error = read
        .execute(&context, json!({"path": "missing.txt"}))
        .await
        .expect_err("missing read");
    let failure = failure_from_anyhow("read_file", &error);
    assert_eq!(failure.code, ToolErrorCode::FILE_NOT_FOUND);
    assert!(!failure.retryable);
    assert!(failure.hint.contains("find_files"));

    let write = registry.get("write_file").unwrap();
    let error = write
        .execute(&context, json!({"path": "../escape", "content": "x"}))
        .await
        .expect_err("path escape");
    assert_eq!(
        failure_from_anyhow("write_file", &error).code,
        ToolErrorCode::PATH_ESCAPE
    );

    let edit = registry.get("edit_file").unwrap();
    let output = edit
        .execute(
            &context,
            json!({"path": "README.md", "old_string": "", "new_string": "x"}),
        )
        .await
        .unwrap();
    assert_eq!(code_of(&output), "INVALID_ARGUMENT");
    assert!(output.content.starts_with("error[INVALID_ARGUMENT]"));
    assert!(output.content.contains("retryable: no"));
    assert!(output.content.contains("hint:"));

    let patch = registry.get("apply_patch").unwrap();
    let output = patch
        .execute(&context, json!({"patch": "not a unified diff"}))
        .await
        .unwrap();
    assert!(output.is_error);
    assert!(
        ["PATCH_CONFLICT", "INVALID_ARGUMENT"].contains(&code_of(&output).as_str()),
        "unexpected patch code {}",
        code_of(&output)
    );

    let git_tool = registry.get("git").unwrap();
    let output = git_tool
        .execute(&context, json!({"action": "unsupported"}))
        .await
        .unwrap();
    assert_eq!(code_of(&output), "INVALID_ARGUMENT");

    let shell = registry.get("shell").unwrap();
    let error = shell
        .execute(&context, json!({}))
        .await
        .expect_err("missing command");
    assert_eq!(
        failure_from_anyhow("shell", &error).code,
        ToolErrorCode::INVALID_ARGUMENT
    );

    let process = registry.get("process").unwrap();
    let output = process
        .execute(
            &context,
            json!({"action": "read", "process_id": "proc-missing"}),
        )
        .await
        .expect("process tool returns a typed failure without a session supervisor");
    assert_eq!(code_of(&output), "PROCESS_FAILED");
    assert!(output.content.starts_with("error[PROCESS_FAILED]"));
    assert!(
        failure_from_anyhow("process", &anyhow::anyhow!("unknown process")).code
            == ToolErrorCode::PROCESS_NOT_FOUND
    );

    let remote = registry.get("remote_read").unwrap();
    let error = remote
        .execute(&context, json!({"path": "README.md"}))
        .await
        .expect_err("remote not configured");
    assert_eq!(
        failure_from_anyhow("remote_read", &error).code,
        ToolErrorCode::REMOTE_NOT_CONFIGURED
    );

    let fetch = registry.get("fetch_url").unwrap();
    let output = fetch
        .execute(
            &context,
            json!({"url": "http://127.0.0.1:9/token?secret=abc"}),
        )
        .await
        .unwrap();
    assert!(output.is_error);
    assert_eq!(code_of(&output), "FETCH_BLOCKED");
    let structured = serde_json::to_string(output.structured.as_ref().unwrap()).unwrap();
    assert!(!structured.contains("secret=abc"));
    assert!(!output.content.contains("secret=abc"));
}

#[tokio::test]
async fn public_failure_contract_is_additive_and_bounded() {
    let legacy = ToolOutput::error_structured(
        "process exited",
        json!({
            "code": "PROCESS_EXITED",
            "process_id": "proc-1",
            "exit_code": 2,
            "tail": "sensitive process output",
            "command": "curl -H 'Authorization: Bearer abcdefghijklmnop'",
            "stdout": "full stdout",
            "stderr": "full stderr",
            "api_key": "sk-abcdefghijklmnop",
            "path": "src/lib.rs"
        }),
    );
    assert!(legacy.is_error);
    assert!(legacy.content.starts_with("error[PROCESS_EXITED]"));
    assert!(legacy.content.contains("retryable: no"));
    let structured = legacy.structured.unwrap();
    assert_eq!(structured["code"], "PROCESS_EXITED");
    assert_eq!(structured["process_id"], "proc-1");
    assert_eq!(structured["exit_code"], 2);
    assert_eq!(structured["path"], "src/lib.rs");
    assert!(structured.get("tail").is_none());
    assert!(structured.get("command").is_none());
    assert!(structured.get("stdout").is_none());
    assert!(structured.get("api_key").is_none());
    let rendered = serde_json::to_string(&structured).unwrap();
    assert!(!rendered.contains("sensitive process output"));
    assert!(!rendered.contains("abcdefghijklmnop"));

    let bare = ToolOutput::error("unknown failure");
    assert_eq!(code_of(&bare), "TOOL_FAILED");
    assert!(bare.content.starts_with("error[TOOL_FAILED]"));

    let long = ToolFailure::new(ToolErrorCode::COMMAND_FAILED, "m".repeat(80_000))
        .with_evidence("e".repeat(80_000));
    assert!(long.message.chars().count() <= 2_000);
    assert!(long.evidence.as_ref().unwrap().chars().count() <= 800);
    assert!(long.render().chars().count() <= 4_000);
}

#[test]
fn catalog_is_documented_unique_and_conservative() {
    let entries = catalog();
    assert!(entries.len() >= 100);
    let mut codes = std::collections::HashSet::new();
    for entry in &entries {
        assert!(codes.insert(entry.code.as_str()));
        assert_eq!(
            entry.code.as_str(),
            entry.code.as_str().to_ascii_uppercase()
        );
        assert!(!entry.category.is_empty());
        assert!(entry.hint.len() >= 20);
        assert!(entry.hint.chars().count() <= 600);
        assert_eq!(entry.retryable, entry.code.default_retryable());
    }
    for required in [
        "TOOL_FAILED",
        "FILE_NOT_FOUND",
        "PATCH_CONFLICT",
        "HASH_MISMATCH",
        "GIT_FAILED",
        "COMMAND_FAILED",
        "PROCESS_EXITED",
        "WAIT_TIMEOUT",
        "LSP_UNAVAILABLE",
        "BROWSER_ERROR",
        "COMPUTER_ERROR",
        "STALE_REF",
        "REMOTE_FAILED",
        "TOOL_OUTPUT_INVALID",
        "MCP_FAILED",
        "FETCH_BLOCKED",
    ] {
        assert!(
            entries.iter().any(|entry| entry.code.as_str() == required),
            "missing code {required}"
        );
    }
    for terminal in [
        "TOOL_FAILED",
        "PATCH_CONFLICT",
        "HASH_MISMATCH",
        "PERMISSION_DENIED",
        "COMMAND_FAILED",
        "REMOTE_WRITE_FAILED",
    ] {
        let expected = terminal == "PATCH_CONFLICT" || terminal == "HASH_MISMATCH";
        assert_eq!(
            ToolErrorCode::from_code(terminal)
                .unwrap()
                .default_retryable(),
            expected,
            "unexpected retryability for {terminal}"
        );
    }
}

#[test]
fn classifier_and_recipes_do_not_depend_on_process_state() {
    assert_eq!(
        classify_message("read_file", "path does not exist"),
        Some(ToolErrorCode::FILE_NOT_FOUND)
    );
    assert_eq!(classify_message("read_file", "novel failure"), None);

    let first_hash = recipes::catalog_hash().to_string();
    let second_hash = recipes::catalog_hash().to_string();
    assert_eq!(first_hash, second_hash);
    assert_eq!(first_hash.len(), 64);

    for slug in [
        "read_many_files",
        "symbol_navigation",
        "safe_edit",
        "test_and_check",
        "long_output_paging",
        "gui_verification",
        "remote_operation",
        "memory_recall",
        "error_recovery",
    ] {
        let recipe = recipes::recipe(slug).expect(slug);
        let detail = recipes::render_recipe_detail(recipe);
        assert!(detail.contains(recipe.title));
        assert!(detail.chars().count() <= recipes::MAX_RECIPE_DETAIL_CHARS);
    }
    let verifier = recipes::recipes_for("verifier", "verification");
    assert!(
        verifier
            .iter()
            .any(|recipe| recipe.slug == "test_and_check")
    );
    assert!(!verifier.iter().any(|recipe| recipe.slug == "safe_edit"));
    let leader = recipes::recipes_for("leader", "generic");
    assert_eq!(leader.len(), recipes::all_recipes().len());
}

#[tokio::test]
async fn activity_projection_is_bounded_newest_first_and_content_free() {
    let dir = TempDir::new().unwrap();
    let repo_dir = dir.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::write(repo_dir.join("README.md"), "activity\n").unwrap();
    git(&repo_dir, &["init", "-q"]);
    git(&repo_dir, &["config", "user.email", "test@example.test"]);
    git(&repo_dir, &["config", "user.name", "Test"]);
    git(&repo_dir, &["add", "."]);
    git(&repo_dir, &["commit", "-q", "-m", "initial"]);

    let engine = Engine::new(Store::open(dir.path()).unwrap());
    let project = engine
        .store()
        .add_project(repo_dir.to_str().unwrap(), Some("repo"))
        .unwrap();
    let session = engine
        .create_session(project.id, "activity".into(), None)
        .await
        .unwrap();
    for index in 0..5 {
        engine
            .append_event(
                &session.id,
                "tool.finished",
                json!({
                    "tool_call_id": format!("call-{index}"),
                    "name": if index % 2 == 0 { "read_file" } else { "edit_file" },
                    "output": "content that must not be projected",
                    "is_error": index == 4,
                    "structured": {
                        "code": "PATCH_CONFLICT",
                        "path": "src/lib.rs",
                        "command": "secret"
                    },
                    "duration_ms": index * 10,
                    "output_bytes": 100 + index,
                    "truncated": index == 3,
                    "approval_risk": "workspace_write",
                    "approval_mode": "smart",
                    "approval_wait_ms": index
                }),
            )
            .await
            .unwrap();
    }
    let activity = engine.session_tool_activity(&session.id, 3).await.unwrap();
    assert_eq!(activity.limit, 3);
    assert_eq!(activity.total, 5);
    assert!(activity.truncated);
    assert_eq!(activity.entries.len(), 3);
    assert_eq!(activity.entries[0].tool_call_id, "call-4");
    assert_eq!(activity.entries[2].tool_call_id, "call-2");
    assert_eq!(
        activity.entries[0].error_code.as_deref(),
        Some("PATCH_CONFLICT")
    );
    assert!(!activity.entries[0].retryable);
    assert_eq!(activity.summary.calls, 3);
    assert_eq!(activity.summary.errors, 1);
    assert_eq!(activity.summary.max_duration_ms, 40);
    assert_eq!(activity.summary.approval_waits, 3);
    assert_eq!(activity.summary.error_codes[0].code, "PATCH_CONFLICT");
    let serialized = serde_json::to_string(&activity).unwrap();
    assert!(!serialized.contains("content that must not be projected"));
    assert!(!serialized.contains("\"command\""));
    assert!(!serialized.contains("secret"));
}

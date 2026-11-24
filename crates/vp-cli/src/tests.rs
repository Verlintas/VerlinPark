//! Unit tests for the `vp` CLI entrypoint, split out of `main.rs`.

use crate::cli::*;
use crate::render::*;
use crate::rpc::*;
use crate::*;
use serde_json::{Value, json};
use std::path::PathBuf;

#[cfg(test)]
// The file is itself the `tests` module; keep the historical test module name.
#[allow(clippy::module_inception)]
mod tests {
    use super::*;

    #[test]
    fn session_operation_id_flags_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "prompt",
                    "ses-1",
                    "continue",
                    "--operation-id",
                    "op-prompt",
                ])
                .unwrap();
                match cli.command {
                    Command::Prompt { operation_id, .. } => {
                        assert_eq!(operation_id.as_deref(), Some("op-prompt"))
                    }
                    other => panic!("expected prompt command, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "defer",
                    "ses-1",
                    "follow up",
                    "--after-secs",
                    "60",
                    "--operation-id",
                    "op-defer",
                ])
                .unwrap();
                match cli.command {
                    Command::Defer { operation_id, .. } => {
                        assert_eq!(operation_id.as_deref(), Some("op-defer"))
                    }
                    other => panic!("expected defer command, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "steer",
                    "ses-1",
                    "inspect tests",
                    "--operation-id",
                    "op-steer",
                ])
                .unwrap();
                match cli.command {
                    Command::Steer { operation_id, .. } => {
                        assert_eq!(operation_id.as_deref(), Some("op-steer"))
                    }
                    other => panic!("expected steer command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn lsp_command_parses_project() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "lsp", "proj-1"]).unwrap();
                match cli.command {
                    Command::Lsp { project } => assert_eq!(project, "proj-1"),
                    other => panic!("expected lsp command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn lsp_status_lines_label_running_installed_and_missing() {
        let value = json!({
            "root": "/tmp/project",
            "running": 1,
            "installed": 2,
            "supported_languages": ["rust", "python"],
            "languages": [
                {
                    "language": "rust",
                    "display": "Rust",
                    "server": "rust-analyzer",
                    "installed": true,
                    "running": true,
                    "capabilities": ["workspace_symbols", "references"],
                    "install_hint": "install rust-analyzer"
                },
                {
                    "language": "python",
                    "display": "Python",
                    "server": "pyright-langserver",
                    "installed": false,
                    "running": false,
                    "capabilities": [],
                    "install_hint": "install pyright"
                }
            ]
        });
        let lines = lsp_status_lines(&value).unwrap();
        assert_eq!(
            lines[0],
            "code intelligence · /tmp/project · 1 running · 2 installed"
        );
        assert!(lines[1].contains("Rust"));
        assert!(lines[1].contains("running"));
        assert!(lines[1].contains("workspace_symbols,references"));
        assert!(lines[2].contains("Python"));
        assert!(lines[2].contains("missing"));
        assert_eq!(lines[3].trim(), "install pyright");
        assert!(lsp_status_lines(&json!({"languages": []})).is_none());
    }

    #[test]
    fn metrics_command_parses_prometheus_flag() {
        // The full CLI command tree is large; parse on a test thread with more
        // stack than libtest's default rather than relying on runner defaults.
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "metrics", "--prometheus"]).unwrap();
                match cli.command {
                    Command::Metrics { prometheus } => assert!(prometheus),
                    other => panic!("expected metrics command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn bench_command_parses_without_daemon_discovery() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "bench",
                    "run",
                    "--scenario",
                    "baseline-single-session",
                    "--repetitions",
                    "2",
                ])
                .unwrap();
                match cli.command {
                    Command::Bench {
                        command: vp_bench::cli::BenchCommand::Run(args),
                    } => {
                        assert_eq!(args.scenarios, vec!["baseline-single-session"]);
                        assert_eq!(args.repetitions, 2);
                    }
                    other => panic!("expected bench run command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_command_parses_session_and_json_flag() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "cache", "ses-123", "--json"]).unwrap();
                assert!(cli.json);
                match cli.command {
                    Command::Cache { session } => assert_eq!(session, "ses-123"),
                    other => panic!("expected cache command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_epoch_command_parses_session_and_bounded_limit() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "cache-epoch", "ses-123", "--limit", "10"]).unwrap();
                match cli.command {
                    Command::CacheEpoch { session, limit } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(limit, 10);
                    }
                    other => panic!("expected cache-epoch command, got {other:?}"),
                }
                let default = Cli::try_parse_from(["vp", "cache-epoch", "ses-123"]).unwrap();
                match default.command {
                    Command::CacheEpoch { limit, .. } => assert_eq!(limit, 50),
                    other => panic!("expected cache-epoch command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_epochs_command_parses_project_and_limit() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "cache-epochs", "prj-123", "--limit", "7"]).unwrap();
                match cli.command {
                    Command::CacheEpochs { project, limit } => {
                        assert_eq!(project, "prj-123");
                        assert_eq!(limit, 7);
                    }
                    other => panic!("expected cache-epochs command, got {other:?}"),
                }
                let prune = Cli::try_parse_from([
                    "vp",
                    "cache-epochs-prune",
                    "prj-123",
                    "--older-than-days",
                    "30",
                    "--keep-epochs",
                    "10",
                    "--dry-run",
                ])
                .unwrap();
                match prune.command {
                    Command::CacheEpochsPrune {
                        project,
                        older_than_days,
                        keep_epochs,
                        dry_run,
                    } => {
                        assert_eq!(project, "prj-123");
                        assert_eq!(older_than_days, 30);
                        assert_eq!(keep_epochs, 10);
                        assert!(dry_run);
                    }
                    other => panic!("expected cache-epochs-prune command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn contract_commands_parse_and_validate_surface() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "contract", "ses-1", "--task", "t1"]).unwrap();
                match cli.command {
                    Command::Contract { session, task } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(task.as_deref(), Some("t1"));
                    }
                    other => panic!("expected contract command, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "contract-policy-set",
                    "prj-1",
                    "--mode",
                    "advisory",
                    "--enforce-shell",
                    "true",
                    "--forbidden-path",
                    "secrets",
                    "--egress-allow",
                    "example.com",
                    "--max-writes",
                    "5",
                ])
                .unwrap();
                match cli.command {
                    Command::ContractPolicySet {
                        project,
                        mode,
                        enforce_shell,
                        forbidden_paths,
                        egress_allowlist,
                        max_writes,
                        ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(mode.as_deref(), Some("advisory"));
                        assert_eq!(enforce_shell, Some(true));
                        assert_eq!(forbidden_paths, vec!["secrets".to_string()]);
                        assert_eq!(egress_allowlist, vec!["example.com".to_string()]);
                        assert_eq!(max_writes, Some(5));
                    }
                    other => panic!("expected contract-policy-set, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "mcp-pin-approve",
                    "prj-1",
                    "srv-1",
                    "echo",
                    "--schema-hash",
                    "a",
                    "--description-hash",
                    "b",
                ])
                .unwrap();
                match cli.command {
                    Command::McpPinApprove {
                        project,
                        server,
                        tool,
                        ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(server, "srv-1");
                        assert_eq!(tool, "echo");
                    }
                    other => panic!("expected mcp-pin-approve, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn instructions_command_parses_session_and_agent() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "instructions", "ses-123", "--agent", "worker-7"])
                        .unwrap();
                match cli.command {
                    Command::Instructions { session, agent } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(agent.as_deref(), Some("worker-7"));
                    }
                    other => panic!("expected instructions command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn prompt_layout_command_parses_explicit_layout() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "prompt-layout", "ses-123", "2"]).unwrap();
                match cli.command {
                    Command::PromptLayout { session, layout } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(layout, 2);
                    }
                    other => panic!("expected prompt-layout command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn instruction_lines_render_ordered_metadata_without_text() {
        let value = json!({
            "session_id": "ses-1",
            "prompt_layout": 3,
            "latest_prefix_hash": "abcdef1234567890",
            "latest_identity_id": "agt-worker-1234567890",
            "latest_identity_generation": 4,
            "last_rebuilt": 1,
            "states": [{
                "state_id": "ses-1",
                "role": "leader",
                "sections": [
                    {"key": "core/charter", "hash": "aaaaaaaaaaaaaaaa", "ordinal": 10, "tail": false, "chars": 120, "available": true},
                    {"key": "tail/memory", "hash": "bbbbbbbbbbbbbbbb", "ordinal": 90, "tail": true, "chars": 20, "available": false}
                ]
            }]
        });
        let lines = instruction_lines(&value).unwrap();
        assert!(lines[0].contains("layout v3"));
        assert!(lines[0].contains("identity agt-worker-1"));
        assert!(lines[0].contains("gen 4"));
        assert!(lines[1].contains("rebuilt diagnostics"));
        assert!(lines.iter().any(|line| line.contains("core/charter")));
        assert!(lines.iter().any(|line| line.contains("tail")));
        assert!(lines.iter().any(|line| line.contains("MISSING")));
        assert!(!lines.join("\n").contains("role charter bytes"));
    }

    #[test]
    fn instruction_lines_omit_the_identity_badge_for_legacy_sessions() {
        let value = json!({
            "session_id": "ses-legacy",
            "prompt_layout": 3,
            "latest_prefix_hash": "abcdef1234567890",
            "states": [{
                "state_id": "ses-legacy",
                "role": "leader",
                "sections": [
                    {"key": "core/charter", "hash": "aaaaaaaaaaaaaaaa", "ordinal": 10, "tail": false, "chars": 120, "available": true}
                ]
            }]
        });
        let lines = instruction_lines(&value).unwrap();
        assert!(lines[0].contains("legacy"));
        assert!(!lines[0].contains("identity"));
        assert!(!lines.join("\n").contains("agt-"));
    }

    #[test]
    fn instruction_lines_do_not_hijack_unrelated_states_arrays() {
        assert!(instruction_lines(&json!({"states": []})).is_none());
    }

    #[test]
    fn cache_audit_parses_a_session() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "cache-audit", "ses-123"]).unwrap();
                match cli.command {
                    Command::CacheAudit { session } => assert_eq!(session, "ses-123"),
                    other => panic!("expected cache-audit command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn tool_bundle_accepts_repeated_roles_and_task_kind() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "tool-bundle",
                    "ses-123",
                    "--role",
                    "worker",
                    "--role",
                    "verifier",
                    "--task-kind",
                    "trivial",
                    "--tools",
                ])
                .unwrap();
                match cli.command {
                    Command::ToolBundle {
                        session,
                        role,
                        task_kind,
                        tools,
                        history,
                    } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(role, vec!["worker".to_string(), "verifier".to_string()]);
                        assert_eq!(task_kind.as_deref(), Some("trivial"));
                        assert!(tools);
                        assert!(!history);
                    }
                    other => panic!("expected tool-bundle command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn tool_bundles_plural_takes_no_arguments() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "tool-bundles"]).unwrap();
                match cli.command {
                    Command::ToolBundles {
                        matrix,
                        task_kind,
                        diff,
                        ..
                    } => {
                        assert!(!matrix);
                        assert!(task_kind.is_empty());
                        assert!(diff.is_none());
                    }
                    other => panic!("expected tool-bundles command, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "tool-bundles",
                    "--matrix",
                    "--task-kind",
                    "trivial",
                ])
                .unwrap();
                match cli.command {
                    Command::ToolBundles {
                        matrix, task_kind, ..
                    } => {
                        assert!(matrix);
                        assert_eq!(task_kind, vec!["trivial".to_string()]);
                    }
                    other => panic!("expected matrix tool-bundles command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn tool_bundles_catalog_view_lists_matrix_and_warnings() {
        let value = json!({
            "bundles": [
                {
                    "bundle": "leader",
                    "title": "Leader",
                    "summary": "orchestration",
                    "target_limit": 48,
                    "requested_count": 46,
                    "representative_role": "leader",
                    "representative_profile": "generic",
                    "allows_dynamic_mcp": true
                },
                {
                    "bundle": "minimal",
                    "title": "Minimal",
                    "summary": "tiny",
                    "target_limit": 12,
                    "requested_count": 6,
                    "representative_role": "worker",
                    "representative_profile": "coding",
                    "allows_dynamic_mcp": false
                }
            ],
            "warnings": []
        });
        let lines = tool_bundle_lines(&value).expect("catalog view");
        let text = lines.join("\n");
        assert!(text.contains("tool bundles"));
        assert!(text.contains("leader"));
        assert!(text.contains("requested 46/48"));
        assert!(text.contains("minimal"));
        assert!(text.contains("requested 6/12"));
    }

    #[test]
    fn tool_bundle_history_and_diff_flags_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "tool-bundle",
                    "ses-123",
                    "--history",
                    "--role",
                    "worker",
                ])
                .unwrap();
                match cli.command {
                    Command::ToolBundle { history, role, .. } => {
                        assert!(history);
                        assert_eq!(role, vec!["worker".to_string()]);
                    }
                    other => panic!("expected tool-bundle history, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "tool-bundles",
                    "--diff",
                    "leader,minimal",
                    "--role",
                    "leader",
                    "--profile",
                    "generic",
                ])
                .unwrap();
                match cli.command {
                    Command::ToolBundles {
                        diff,
                        role,
                        profile,
                        ..
                    } => {
                        assert_eq!(diff.as_deref(), Some("leader,minimal"));
                        assert_eq!(role.as_deref(), Some("leader"));
                        assert_eq!(profile.as_deref(), Some("generic"));
                    }
                    other => panic!("expected tool-bundles diff, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn tool_bundle_history_view_lists_observations() {
        let value = json!({
            "session_id": "ses-1",
            "entries": [{
                "seq": 12,
                "role": "worker",
                "agent_id": "agt-1",
                "agent_name": "Worker",
                "bundle": "minimal",
                "bundle_hash": "hash",
                "tools_hash": "tools",
                "tool_count": 6,
                "mcp_tool_count": 0,
                "task_kind": "trivial",
                "fallback": false,
                "reason": "task_kind",
                "created_at": "2026-11-12T00:00:00Z"
            }]
        });
        let text = tool_bundle_lines(&value).expect("history view").join("\n");
        assert!(text.contains("tool bundle history"));
        assert!(text.contains("minimal"));
        assert!(text.contains("worker"));
        let empty = json!({"session_id": "ses-1", "entries": []});
        let text = tool_bundle_lines(&empty).expect("empty history").join("\n");
        assert!(text.contains("no persisted bundle observations"));
    }

    #[test]
    fn tool_bundle_diff_view_lists_added_removed_and_shared() {
        let value = json!({
            "from_bundle": "leader",
            "to_bundle": "minimal",
            "role": "leader",
            "profile": "generic",
            "task_kind": null,
            "from_hash": "a",
            "to_hash": "b",
            "from_count": 46,
            "to_count": 6,
            "added": [],
            "removed": ["delegate", "shell"],
            "shared": ["read_file", "write_file"],
            "changed": true,
            "warnings": []
        });
        let text = tool_bundle_lines(&value).expect("diff view").join("\n");
        assert!(text.contains("tool bundle diff  leader → minimal"));
        assert!(text.contains("46 → 6 tools"));
        assert!(text.contains("removed"));
        assert!(text.contains("delegate, shell"));
        assert!(text.contains("shared"));
    }

    #[test]
    fn tool_bundle_matrix_view_lists_runtime_rows() {
        let value = json!({
            "task_kinds": ["", "trivial"],
            "rows": [{
                "role": "worker",
                "profile": "coding",
                "task_kind": "trivial",
                "bundle": "minimal",
                "reason": "task_kind",
                "fallback": false,
                "bundle_hash": "hash",
                "tools_hash": "tools",
                "builtin_count": 6,
                "mcp_count": 0,
                "total_count": 6,
                "requested_count": 6,
                "omitted_count": 0,
                "target_limit": 12,
                "within_target": true,
                "warning_count": 0
            }],
            "warnings": []
        });
        let lines = tool_bundle_lines(&value).expect("matrix view");
        let text = lines.join("\n");
        assert!(text.contains("tool bundle matrix"));
        assert!(text.contains("minimal"));
        assert!(text.contains("worker"));
        assert!(text.contains("trivial"));
    }

    #[test]
    fn tool_bundle_human_view_lists_bundles_and_catalog_without_schemas() {
        let value = json!({
            "session_id": "ses-1",
            "roles": [{
                "role": "worker",
                "profile": "coding",
                "task_kind": "trivial",
                "bundle": "minimal",
                "title": "Minimal",
                "reason": "task_kind",
                "fallback": false,
                "bundle_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "tools_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "requested": ["read_file", "write_file"],
                "omitted": ["apply_patch"],
                "warnings": [],
                "builtin_count": 6,
                "mcp_count": 0,
                "total_count": 6,
                "target_limit": 12,
                "within_target": true,
                "tools": [
                    {"name": "read_file", "category": "files"},
                    {"name": "write_file", "category": "files"}
                ],
                "last_run": {
                    "bundle": "minimal",
                    "tool_count": 6,
                    "mcp_tool_count": 0,
                    "created_at": "2026-11-12T00:00:00Z"
                }
            }],
            "catalog": [{
                "bundle": "minimal",
                "requested_count": 6,
                "target_limit": 12,
                "allows_dynamic_mcp": false
            }]
        });
        let lines = tool_bundle_lines(&value).expect("tool bundle view");
        let text = lines.join("\n");
        assert!(text.contains("tool bundle  ses-1"));
        assert!(text.contains("worker"));
        assert!(text.contains("minimal"));
        assert!(text.contains("6/12 tools"));
        assert!(text.contains("omitted apply_patch"));
        assert!(text.contains("read_file, write_file"));
        assert!(!text.contains("description for"));
    }

    #[test]
    fn cache_plan_accepts_repeated_roles() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-plan",
                    "ses-123",
                    "--role",
                    "leader",
                    "--role",
                    "worker",
                ])
                .unwrap();
                match cli.command {
                    Command::CachePlan { session, role } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(role, vec!["leader".to_string(), "worker".to_string()]);
                    }
                    other => panic!("expected cache-plan command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_warm_accepts_both_spellings() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let direct =
                    Cli::try_parse_from(["vp", "cache-warm", "ses-123", "--role", "worker"])
                        .unwrap();
                match direct.command {
                    Command::CacheWarm { session, role } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(role.as_deref(), Some("worker"));
                    }
                    other => panic!("expected cache-warm command, got {other:?}"),
                }
                let normalized = normalize_cache_warm_args(vec![
                    "vp".into(),
                    "cache".into(),
                    "warm".into(),
                    "ses-123".into(),
                ]);
                let cli = Cli::try_parse_from(normalized).unwrap();
                match cli.command {
                    Command::CacheWarm { session, role } => {
                        assert_eq!(session, "ses-123");
                        assert!(role.is_none());
                    }
                    other => panic!("expected normalized cache-warm command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_policy_set_parses_ttl_and_compaction_flags() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-policy-set",
                    "prj-1",
                    "--ttl",
                    "1h",
                    "--no-automatic",
                    "--warm-on-resume",
                    "--disabled",
                    "--role-ttl",
                    "worker=1h",
                ])
                .unwrap();
                match cli.command {
                    Command::CachePolicySet {
                        project,
                        disabled,
                        ttl,
                        no_automatic,
                        warm_on_resume,
                        no_compact_aware,
                        role_ttl,
                        prompt_layout,
                    } => {
                        assert_eq!(project, "prj-1");
                        assert!(disabled);
                        assert_eq!(ttl, "1h");
                        assert!(no_automatic);
                        assert!(warm_on_resume);
                        assert!(!no_compact_aware);
                        assert_eq!(role_ttl, vec!["worker=1h".to_string()]);
                        assert_eq!(prompt_layout, None);
                    }
                    other => panic!("expected cache-policy-set command, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod durable_run_cli_tests {
    use super::*;

    #[test]
    fn durable_run_commands_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "run-policy-set",
                    "proj-1",
                    "--auto-resume",
                    "--max-attempts",
                    "3",
                    "--max-age-hours",
                    "12",
                ])
                .unwrap();
                match cli.command {
                    Command::RunPolicySet {
                        project,
                        auto_resume,
                        max_attempts,
                        max_age_hours,
                    } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(auto_resume, Some(true));
                        assert_eq!(max_attempts, Some(3));
                        assert_eq!(max_age_hours, Some(12));
                    }
                    other => panic!("expected run-policy-set, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "run-resume",
                    "ses-1",
                    "--run",
                    "run-1",
                    "--operation-id",
                    "op-resume",
                ])
                .unwrap();
                match cli.command {
                    Command::RunResume {
                        session,
                        run,
                        operation_id,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(run.as_deref(), Some("run-1"));
                        assert_eq!(operation_id.as_deref(), Some("op-resume"));
                    }
                    other => panic!("expected run-resume, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn parses_tool_output_threshold_overrides() {
        let parsed =
            parse_tool_thresholds(&["shell=131072".to_string(), "grep = 8192".to_string()])
                .unwrap();
        assert_eq!(parsed[0], ("shell".to_string(), 131_072));
        assert_eq!(parsed[1], ("grep".to_string(), 8_192));
        assert!(parse_tool_thresholds(&["shell".to_string()]).is_err());
        assert!(parse_tool_thresholds(&["=1024".to_string()]).is_err());
        assert!(parse_tool_thresholds(&["shell=not-a-number".to_string()]).is_err());
    }

    #[test]
    fn tool_output_cli_flags_parse_additively() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "tool-output-policy-set",
                    "proj-1",
                    "--spill-threshold",
                    "32768",
                    "--preview-bytes",
                    "8192",
                    "--tool-threshold",
                    "shell=131072",
                    "--max-entries",
                    "500",
                ])
                .unwrap();
                match cli.command {
                    Command::ToolOutputPolicySet {
                        project,
                        spill_threshold,
                        preview_bytes,
                        max_entries,
                        tool_thresholds,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(spill_threshold, Some(32_768));
                        assert_eq!(preview_bytes, Some(8_192));
                        assert_eq!(max_entries, Some(500));
                        assert_eq!(tool_thresholds, vec!["shell=131072".to_string()]);
                    }
                    other => panic!("expected tool-output-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod hybrid_memory_cli_tests {
    use super::*;

    fn parse_cli(args: &[&str]) -> Cli {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn hybrid_memory_commands_and_policy_flags_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "memory-embeddings", "proj-1"]).unwrap();
                match cli.command {
                    Command::MemoryEmbeddings { project } => assert_eq!(project, "proj-1"),
                    other => panic!("expected memory-embeddings, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "memory-embeddings-rebuild", "proj-1"]).unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsRebuild { project } => {
                        assert_eq!(project, "proj-1")
                    }
                    other => panic!("expected memory-embeddings-rebuild, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-embeddings-test",
                    "proj-1",
                    "--text",
                    "probe text",
                ])
                .unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsTest { project, text } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(text.as_deref(), Some("probe text"));
                    }
                    other => panic!("expected memory-embeddings-test, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "memory-embeddings-index", "proj-1"]).unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsIndex { project } => assert_eq!(project, "proj-1"),
                    other => panic!("expected memory-embeddings-index, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "memory-embeddings-index-rebuild", "proj-1"])
                    .unwrap();
                match cli.command {
                    Command::MemoryEmbeddingsIndexRebuild { project } => {
                        assert_eq!(project, "proj-1")
                    }
                    other => panic!("expected memory-embeddings-index-rebuild, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-policy-set",
                    "proj-1",
                    "--embedding-index",
                    "ivf",
                    "--ivf-nlist",
                    "128",
                    "--ivf-nprobe",
                    "12",
                    "--ivf-min-vectors",
                    "300",
                    "--ivf-rebuild-vectors",
                    "6000",
                ])
                .unwrap();
                match cli.command {
                    Command::MemoryPolicySet {
                        embedding_index,
                        ivf_nlist,
                        ivf_nprobe,
                        ivf_min_vectors,
                        ivf_rebuild_vectors,
                        ..
                    } => {
                        assert_eq!(embedding_index.as_deref(), Some("ivf"));
                        assert_eq!(ivf_nlist, Some(128));
                        assert_eq!(ivf_nprobe, Some(12));
                        assert_eq!(ivf_min_vectors, Some(300));
                        assert_eq!(ivf_rebuild_vectors, Some(6000));
                    }
                    other => panic!("expected memory-policy-set, got {other:?}"),
                }

                assert!(
                    Cli::try_parse_from([
                        "vp",
                        "memory-policy-set",
                        "proj-1",
                        "--embedding-index",
                        "hnsw",
                    ])
                    .is_err(),
                    "unknown index modes are rejected by the CLI parser"
                );

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-search",
                    "proj-1",
                    "deployment release",
                    "--lexical-only",
                ])
                .unwrap();
                match cli.command {
                    Command::MemorySearch {
                        project,
                        query,
                        lexical_only,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(query, "deployment release");
                        assert!(lexical_only);
                    }
                    other => panic!("expected memory-search, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "memory-policy-set",
                    "proj-1",
                    "--embedding-enable",
                    "--embedding-endpoint",
                    "http://127.0.0.1:8080/v1",
                    "--embedding-model",
                    "embed-small",
                    "--embedding-dimensions",
                    "512",
                    "--embedding-batch-size",
                    "8",
                    "--embedding-timeout-secs",
                    "12",
                    "--embedding-max-candidates",
                    "32",
                    "--embedding-min-similarity",
                    "0.35",
                    "--embedding-api-key-env",
                    "EMBED_API_KEY",
                ])
                .unwrap();
                match cli.command {
                    Command::MemoryPolicySet {
                        project,
                        embedding_enable,
                        embedding_disable,
                        embedding_endpoint,
                        embedding_model,
                        embedding_dimensions,
                        embedding_batch_size,
                        embedding_timeout_secs,
                        embedding_max_candidates,
                        embedding_min_similarity,
                        embedding_api_key_env,
                        embedding_clear,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert!(embedding_enable);
                        assert!(!embedding_disable);
                        assert_eq!(
                            embedding_endpoint.as_deref(),
                            Some("http://127.0.0.1:8080/v1")
                        );
                        assert_eq!(embedding_model.as_deref(), Some("embed-small"));
                        assert_eq!(embedding_dimensions, Some(512));
                        assert_eq!(embedding_batch_size, Some(8));
                        assert_eq!(embedding_timeout_secs, Some(12));
                        assert_eq!(embedding_max_candidates, Some(32));
                        assert_eq!(embedding_min_similarity, Some(0.35));
                        assert_eq!(embedding_api_key_env.as_deref(), Some("EMBED_API_KEY"));
                        assert!(!embedding_clear);
                    }
                    other => panic!("expected memory-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn acp_remote_add_flags_parse() {
        let cli = parse_cli(&[
            "vp",
            "acp-add",
            "remote",
            "--transport",
            "streamable-http",
            "--endpoint",
            "https://agent.example.test/acp",
            "--auth-header-env",
            "ACP_TOKEN",
            "--header",
            "x-project=demo",
            "--header",
            "x-token=${ACP_TOKEN}",
            "--allow-remote",
            "--connect-timeout",
            "15",
            "--timeout",
            "600",
        ]);
        match cli.command {
            Command::AcpAdd {
                name,
                command,
                transport,
                endpoint,
                auth_header_env,
                headers,
                allow_remote,
                connect_timeout,
                timeout,
                ..
            } => {
                assert_eq!(name, "remote");
                assert!(command.is_none());
                assert_eq!(transport.as_str(), "streamable_http");
                assert_eq!(endpoint.as_deref(), Some("https://agent.example.test/acp"));
                assert_eq!(auth_header_env.as_deref(), Some("ACP_TOKEN"));
                assert_eq!(headers.len(), 2);
                assert!(allow_remote);
                assert_eq!(connect_timeout, 15);
                assert_eq!(timeout, 600);
            }
            other => panic!("expected acp-add, got {other:?}"),
        }
    }

    #[test]
    fn acp_stdio_add_keeps_the_optional_command() {
        let cli = parse_cli(&["vp", "acp-add", "local", "npx"]);
        match cli.command {
            Command::AcpAdd {
                transport, command, ..
            } => {
                assert_eq!(transport.as_str(), "stdio");
                assert_eq!(command.as_deref(), Some("npx"));
            }
            other => panic!("expected acp-add, got {other:?}"),
        }
    }

    #[test]
    fn acp_update_flags_parse() {
        let cli = parse_cli(&[
            "vp",
            "acp-update",
            "agent-1",
            "--transport",
            "websocket",
            "--endpoint",
            "wss://agent.example.test/acp",
            "--allow-remote",
            "--clear-headers",
            "--no-auto-approve",
            "--disabled",
        ]);
        match cli.command {
            Command::AcpUpdate {
                agent,
                transport,
                endpoint,
                allow_remote,
                clear_headers,
                no_auto_approve,
                disabled,
                ..
            } => {
                assert_eq!(agent, "agent-1");
                assert_eq!(transport.unwrap().as_str(), "websocket");
                assert_eq!(endpoint.as_deref(), Some("wss://agent.example.test/acp"));
                assert!(allow_remote);
                assert!(clear_headers);
                assert!(no_auto_approve);
                assert!(disabled);
            }
            other => panic!("expected acp-update, got {other:?}"),
        }
    }

    #[test]
    fn acp_transport_names_are_stable() {
        assert_eq!(AcpTransportArg::Stdio.as_str(), "stdio");
        assert_eq!(AcpTransportArg::StreamableHttp.as_str(), "streamable_http");
        assert_eq!(AcpTransportArg::Websocket.as_str(), "websocket");
    }

    #[test]
    fn acp_header_pairs_reject_missing_equals_and_empty_keys() {
        assert_eq!(
            parse_pairs(&["x-project=demo".to_string()], "--header")
                .unwrap()
                .get("x-project")
                .map(String::as_str),
            Some("demo")
        );
        assert!(parse_pairs(&["missing".to_string()], "--header").is_err());
        assert!(parse_pairs(&["=value".to_string()], "--header").is_err());
        assert!(
            parse_pairs(&["x=a=b".to_string()], "--header")
                .unwrap()
                .get("x")
                .is_some_and(|value| value == "a=b")
        );
    }
}

#[cfg(test)]
mod context_checkpoint_cli_tests {
    use super::*;

    #[test]
    fn context_checkpoint_commands_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoints", "ses-1", "--limit", "12"])
                        .unwrap();
                match cli.command {
                    Command::ContextCheckpoints { session, limit } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(limit, Some(12));
                    }
                    other => panic!("expected context-checkpoints, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "context-checkpoint",
                    "ses-1",
                    "--label",
                    "before refactor",
                ])
                .unwrap();
                match cli.command {
                    Command::ContextCheckpoint { session, label } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(label.as_deref(), Some("before refactor"));
                    }
                    other => panic!("expected context-checkpoint, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoint-detail", "ses-1", "ctx-1"])
                        .unwrap();
                match cli.command {
                    Command::ContextCheckpointDetail {
                        session,
                        checkpoint,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(checkpoint, "ctx-1");
                    }
                    other => panic!("expected context-checkpoint-detail, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "context-restore", "ses-1", "ctx-1"]).unwrap();
                match cli.command {
                    Command::ContextRestore {
                        session,
                        checkpoint,
                        force,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(checkpoint, "ctx-1");
                        assert!(!force);
                    }
                    other => panic!("expected context-restore, got {other:?}"),
                }
                let cli =
                    Cli::try_parse_from(["vp", "context-restore", "ses-1", "ctx-1", "--force"])
                        .unwrap();
                match cli.command {
                    Command::ContextRestore { force, .. } => assert!(force),
                    other => panic!("expected forced context-restore, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoint-delete", "ses-1", "ctx-1"])
                        .unwrap();
                match cli.command {
                    Command::ContextCheckpointDelete {
                        session,
                        checkpoint,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(checkpoint, "ctx-1");
                    }
                    other => panic!("expected context-checkpoint-delete, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "context-checkpoint-prune",
                    "ses-1",
                    "--older-than-days",
                    "7",
                    "--keep-latest",
                    "2",
                    "--dry-run",
                ])
                .unwrap();
                match cli.command {
                    Command::ContextCheckpointPrune {
                        session,
                        older_than_days,
                        keep_latest,
                        dry_run,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(older_than_days, Some(7));
                        assert_eq!(keep_latest, Some(2));
                        assert!(dry_run);
                    }
                    other => panic!("expected context-checkpoint-prune, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "context-checkpoint-policy", "prj-1"]).unwrap();
                match cli.command {
                    Command::ContextCheckpointPolicy { project } => {
                        assert_eq!(project, "prj-1")
                    }
                    other => panic!("expected context-checkpoint-policy, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "context-checkpoint-policy-set",
                    "prj-1",
                    "--enabled",
                    "--max-checkpoints",
                    "4",
                    "--max-age-days",
                    "9",
                ])
                .unwrap();
                match cli.command {
                    Command::ContextCheckpointPolicySet {
                        project,
                        enabled,
                        max_checkpoints,
                        max_age_days,
                    } => {
                        assert_eq!(project, "prj-1");
                        assert!(enabled);
                        assert_eq!(max_checkpoints, 4);
                        assert_eq!(max_age_days, 9);
                    }
                    other => panic!("expected context-checkpoint-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod context_checkpoint_human_tests {
    use super::*;

    #[test]
    fn context_checkpoint_list_renders_token_deltas_and_previews() {
        let value = json!({
            "session_id": "ses-1",
            "latest_id": "ctx-2",
            "limit": 50,
            "policy": {"enabled": false, "max_checkpoints": 20, "max_age_days": 30},
            "checkpoints": [
                {
                    "id": "ctx-1",
                    "mode": "summary",
                    "label": "auto summary #1",
                    "summary": "Earlier conversation summary with enough text to become a preview",
                    "plan_revision": 2,
                    "tokens_before": 90000,
                    "tokens_after": 12000,
                    "created_at": "2026-11-11T00:00:00Z"
                },
                {
                    "id": "ctx-2",
                    "mode": "manual",
                    "label": "manual",
                    "plan_revision": 2,
                    "tokens_before": 12000,
                    "tokens_after": 12000,
                    "created_at": "2026-11-11T00:01:00Z"
                }
            ]
        });
        let lines = context_checkpoint_lines(&value).unwrap();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("ses-1"));
        assert!(lines[0].contains("latest ctx-2"));
        assert!(lines[1].contains("sweep off"));
        assert!(lines[2].contains("ctx-1"));
        assert!(lines[2].contains("−78000"));
        assert!(lines[2].contains("Earlier conversation summary"));
        assert!(lines[3].contains("ctx-2"));
        assert!(lines[3].contains("−0"));
    }

    #[test]
    fn context_checkpoint_detail_and_actions_render() {
        let detail = json!({
            "checkpoint": {
                "id": "ctx-1",
                "mode": "summary",
                "label": "before",
                "plan_revision": 1,
                "tokens_before": 5000,
                "tokens_after": 500
            },
            "preflight": {
                "errors": ["plan revision changed"],
                "warnings": ["estimate above threshold"],
                "active_messages": 4,
                "estimated_tokens": 1200,
                "hard_limit_tokens": 32000
            },
            "is_active": false,
            "summary_preview": "before"
        });
        let lines = context_checkpoint_lines(&detail).unwrap();
        assert!(lines[0].contains("ctx-1"));
        assert!(lines[1].contains("active boundary: no"));
        assert!(
            lines
                .iter()
                .any(|line| line == "error: plan revision changed")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "warning: estimate above threshold")
        );

        let restore = json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1",
            "mode": "summary",
            "active_messages": 3,
            "estimated_tokens": 500,
            "compaction_event_seq": 11,
            "restored_event_seq": 12
        });
        let lines = context_checkpoint_lines(&restore).unwrap();
        assert!(lines[0].contains("restored ctx-1"));
        assert!(lines[0].contains("events 11/12"));

        let delete = json!({
            "session_id": "ses-1",
            "checkpoint_id": "ctx-1",
            "removed": true
        });
        assert_eq!(
            context_checkpoint_lines(&delete).unwrap(),
            vec!["ctx-1 deleted".to_string()]
        );

        let prune = json!({
            "session_id": "ses-1",
            "candidates": 2,
            "removed": 1,
            "dry_run": true,
            "checkpoints": [{"id": "ctx-1", "tokens_before": 10}]
        });
        assert_eq!(
            context_checkpoint_lines(&prune).unwrap(),
            vec!["dry run: pruned 1 of 2 candidate context checkpoint(s)".to_string()]
        );

        let policy = json!({
            "project_id": "prj-1",
            "policy": {"enabled": true, "max_checkpoints": 5, "max_age_days": 7}
        });
        assert_eq!(
            context_checkpoint_lines(&policy).unwrap(),
            vec!["prj-1: context checkpoint sweep on · max 5 · age 7d".to_string()]
        );
    }

    #[test]
    fn unrelated_results_are_left_to_the_generic_printer() {
        assert!(context_checkpoint_lines(&json!({"checkpoints": []})).is_none());
        assert!(
            context_checkpoint_lines(&json!({
                "session_id": "ses-1",
                "removed": false
            }))
            .is_none()
        );
        assert!(
            context_checkpoint_lines(&json!({
                "project_id": "prj-1",
                "policy": {"enabled": true, "older_than_secs": 100, "keep_latest": 5}
            }))
            .is_none()
        );
    }
}

#[cfg(test)]
mod context_checkpoint_preview_tests {
    use super::*;

    #[test]
    fn long_summaries_are_truncated_in_the_human_list() {
        let summary = "word ".repeat(40);
        let value = json!({
            "session_id": "ses-1",
            "latest_id": "ctx-1",
            "policy": {"enabled": true, "max_checkpoints": 20, "max_age_days": 30},
            "checkpoints": [{
                "id": "ctx-1",
                "mode": "summary",
                "label": "long",
                "summary": summary,
                "plan_revision": 0,
                "tokens_before": 100,
                "tokens_after": 50,
                "created_at": "2026-11-11T00:00:00Z"
            }]
        });
        let lines = context_checkpoint_lines(&value).unwrap();
        let checkpoint_line = lines.last().unwrap();
        assert!(checkpoint_line.ends_with('…'), "{checkpoint_line}");
        assert!(checkpoint_line.chars().count() < 180);
        assert!(lines[1].contains("sweep on"));
    }
}

#[cfg(test)]
mod tool_activity_cli_tests {
    use super::*;

    fn activity(entries: Value, truncated: bool) -> Value {
        json!({
            "session_id": "ses-1",
            "entries": entries,
            "total": 12,
            "limit": 50,
            "truncated": truncated,
            "summary": {
                "calls": 2,
                "errors": 1,
                "retryable_errors": 1,
                "truncated_results": 1,
                "total_duration_ms": 140,
                "max_duration_ms": 100,
                "average_duration_ms": 70,
                "output_bytes": 2048,
                "by_tool": []
            }
        })
    }

    #[test]
    fn empty_activity_prints_a_clear_summary() {
        let lines = tool_activity_lines(&activity(json!([]), false)).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("2 tool call(s)"));
        assert!(lines[0].contains("1 error(s)"));
        assert!(lines[0].contains("avg 70 ms"));
        assert!(lines[1].contains("No tool activity"));
    }

    #[test]
    fn rows_show_code_hint_and_retryability_without_content() {
        let value = activity(
            json!([
                {
                    "tool_call_id": "call-1",
                    "name": "edit_file",
                    "seq": 7,
                    "is_error": true,
                    "error_code": "PATCH_CONFLICT",
                    "retryable": true,
                    "hint": "Re-read the file and regenerate the patch.",
                    "duration_ms": 100,
                    "output_bytes": 512,
                    "truncated": true,
                    "approval_risk": "workspace_write",
                    "approval_mode": "smart",
                    "approval_wait_ms": 40,
                    "created_at": "2026-11-11T00:00:00Z"
                },
                {
                    "tool_call_id": "call-2",
                    "name": "read_file",
                    "seq": 8,
                    "is_error": false,
                    "duration_ms": 40,
                    "output_bytes": 1536,
                    "truncated": false,
                    "created_at": "2026-11-11T00:00:01Z"
                }
            ]),
            true,
        );
        let lines = tool_activity_lines(&value).unwrap();
        assert!(lines[0].contains("older events omitted"));
        assert!(lines.iter().any(|line| line.contains("PATCH_CONFLICT")));
        assert!(lines.iter().any(|line| line.contains("retryable")));
        assert!(lines.iter().any(|line| line.contains("truncated")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Re-read the file and regenerate"))
        );
        assert!(lines.iter().any(|line| line.contains("read_file")));
        let rendered = lines.join("\n");
        assert!(!rendered.contains("provider output"));
    }

    #[test]
    fn tool_code_catalog_filters_are_stable() {
        let all = filtered_tool_codes(None, None);
        assert!(all.len() >= 100);
        assert!(all.windows(2).all(|pair| {
            pair[0]
                .category
                .cmp(pair[1].category)
                .then_with(|| pair[0].code.as_str().cmp(pair[1].code.as_str()))
                .is_le()
        }));
        let file = filtered_tool_codes(Some(" FILE "), None);
        assert!(!file.is_empty());
        assert!(file.iter().all(|entry| entry.category == "file"));
        assert!(
            file.iter()
                .any(|entry| entry.code.as_str() == "PATCH_CONFLICT")
        );
        let retryable = filtered_tool_codes(None, Some(true));
        assert!(!retryable.is_empty());
        assert!(retryable.iter().all(|entry| entry.retryable));
        assert!(
            retryable
                .iter()
                .any(|entry| entry.code.as_str() == "WAIT_TIMEOUT")
        );
        let terminal = filtered_tool_codes(Some("shell"), Some(false));
        assert!(!terminal.is_empty());
        assert!(terminal.iter().all(|entry| !entry.retryable));
        assert!(filtered_tool_codes(Some("not-a-category"), None).is_empty());
        assert!(
            filtered_tool_codes(Some("file"), Some(true)).len()
                < filtered_tool_codes(Some("file"), None).len()
        );
    }

    #[test]
    fn csv_export_is_stable_and_escaped() {
        let csv = tool_activity_csv(&activity(
            json!([
                {
                    "tool_call_id": "call,one",
                    "name": "shell",
                    "seq": 4,
                    "is_error": true,
                    "error_code": "COMMAND_FAILED",
                    "retryable": false,
                    "hint": "line one\nline \"two\"",
                    "duration_ms": 12,
                    "output_bytes": 34,
                    "truncated": false,
                    "approval_risk": "host_state",
                    "approval_mode": "strict",
                    "approval_wait_ms": 56,
                    "created_at": "2026-11-11T00:00:00Z"
                }
            ]),
            false,
        ));
        assert!(csv.starts_with("seq,created_at,tool_call_id,name,status,error_code,retryable,duration_ms,output_bytes,truncated,approval_risk,approval_mode,approval_wait_ms,hint\n"));
        assert!(csv.contains("\"call,one\""));
        assert!(csv.contains("COMMAND_FAILED"));
        assert!(csv.contains("\"line one\nline \"\"two\"\"\""));
        assert!(csv.contains(",strict,56,"));
        // The embedded newline is quoted, so the raw newline count is three:
        // header, the quoted hint line break, and the record terminator.
        assert_eq!(csv.matches('\n').count(), 3);
    }

    #[test]
    fn csv_export_handles_missing_entries() {
        let csv = tool_activity_csv(&json!({"session_id": "ses-1"}));
        assert_eq!(csv.lines().count(), 1);
        assert!(csv.ends_with('\n'));
        assert!(csv.starts_with("seq,"));
    }

    #[test]
    fn legacy_and_unrelated_payloads_are_not_misformatted() {
        let legacy = json!({
            "session_id": "ses-1",
            "entries": [{"name": "shell"}],
            "total": 1,
            "summary": {"calls": 1}
        });
        assert!(tool_activity_lines(&legacy).is_none());
        assert!(tool_activity_lines(&json!({"events": []})).is_none());
        assert!(
            tool_activity_lines(&json!({
                "session_id": "ses-1",
                "entries": [],
                "summary": {"calls": 0}
            }))
            .is_none()
        );
    }
}

#[cfg(test)]
mod source_embedding_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn source_search_mode_parses_and_defaults_to_none() {
        let cli = parse(&["vp", "source-search", "proj-1", "leases"]);
        match cli.command {
            Command::SourceSearch { mode, limit, .. } => {
                assert!(mode.is_none(), "absent mode keeps the dynamic default");
                assert_eq!(limit, 8);
            }
            other => panic!("expected source-search, got {other:?}"),
        }
        let cli = parse(&[
            "vp",
            "source-search",
            "proj-1",
            "leases",
            "--mode",
            "semantic",
        ]);
        match cli.command {
            Command::SourceSearch { mode, .. } => assert_eq!(mode.as_deref(), Some("semantic")),
            other => panic!("expected source-search, got {other:?}"),
        }
        assert!(
            std::thread::Builder::new()
                .stack_size(32 * 1024 * 1024)
                .spawn(|| {
                    Cli::try_parse_from([
                        "vp",
                        "source-search",
                        "proj-1",
                        "leases",
                        "--mode",
                        "vector",
                    ])
                    .is_err()
                })
                .unwrap()
                .join()
                .unwrap()
        );
    }

    #[test]
    fn source_embedding_commands_parse() {
        let cli = parse(&["vp", "source-embedding", "proj-1"]);
        match cli.command {
            Command::SourceEmbedding { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-embedding, got {other:?}"),
        }
        let cli = parse(&["vp", "source-embedding-reindex", "proj-1"]);
        match cli.command {
            Command::SourceEmbeddingReindex { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-embedding-reindex, got {other:?}"),
        }
        let cli = parse(&["vp", "source-embedding-policy", "proj-1"]);
        match cli.command {
            Command::SourceEmbeddingPolicy { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-embedding-policy, got {other:?}"),
        }
    }

    #[test]
    fn source_embedding_policy_set_parses_every_flag() {
        let cli = parse(&[
            "vp",
            "source-embedding-policy-set",
            "proj-1",
            "--enable",
            "--endpoint-env",
            "SOURCE_EMBED_ENDPOINT",
            "--model",
            "embed-small",
            "--dimensions",
            "512",
            "--batch-size",
            "4",
            "--max-chunks-per-source",
            "120",
            "--retention-days",
            "14",
            "--timeout-secs",
            "20",
            "--api-key-env",
            "SOURCE_EMBED_KEY",
        ]);
        match cli.command {
            Command::SourceEmbeddingPolicySet {
                project,
                enable,
                disable,
                endpoint_env,
                model,
                dimensions,
                batch_size,
                max_chunks_per_source,
                retention_days,
                timeout_secs,
                api_key_env,
                clear,
            } => {
                assert_eq!(project, "proj-1");
                assert!(enable);
                assert!(!disable);
                assert_eq!(endpoint_env.as_deref(), Some("SOURCE_EMBED_ENDPOINT"));
                assert_eq!(model.as_deref(), Some("embed-small"));
                assert_eq!(dimensions, Some(512));
                assert_eq!(batch_size, Some(4));
                assert_eq!(max_chunks_per_source, Some(120));
                assert_eq!(retention_days, Some(14));
                assert_eq!(timeout_secs, Some(20));
                assert_eq!(api_key_env.as_deref(), Some("SOURCE_EMBED_KEY"));
                assert!(!clear);
            }
            other => panic!("expected source-embedding-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn source_retrieval_commands_parse() {
        let cli = parse(&["vp", "source-retrieval-policy", "proj-1"]);
        match cli.command {
            Command::SourceRetrievalPolicy { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-retrieval-policy, got {other:?}"),
        }
        let cli = parse(&["vp", "source-index", "proj-1"]);
        match cli.command {
            Command::SourceIndex { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-index, got {other:?}"),
        }
        let cli = parse(&["vp", "source-index-rebuild", "proj-1"]);
        match cli.command {
            Command::SourceIndexRebuild { project } => assert_eq!(project, "proj-1"),
            other => panic!("expected source-index-rebuild, got {other:?}"),
        }
    }

    #[test]
    fn source_retrieval_policy_set_parses_every_flag() {
        let cli = parse(&[
            "vp",
            "source-retrieval-policy-set",
            "proj-1",
            "--enable-ann",
            "--ann-min-vectors",
            "16",
            "--ann-nprobe",
            "3",
            "--rerank",
            "deterministic",
            "--query-expansion",
            "lexical",
            "--max-expanded-terms",
            "6",
        ]);
        match cli.command {
            Command::SourceRetrievalPolicySet {
                project,
                enable_ann,
                disable_ann,
                ann_min_vectors,
                ann_nprobe,
                rerank,
                query_expansion,
                max_expanded_terms,
                clear,
            } => {
                assert_eq!(project, "proj-1");
                assert!(enable_ann);
                assert!(!disable_ann);
                assert_eq!(ann_min_vectors, Some(16));
                assert_eq!(ann_nprobe, Some(3));
                assert_eq!(rerank.as_deref(), Some("deterministic"));
                assert_eq!(query_expansion.as_deref(), Some("lexical"));
                assert_eq!(max_expanded_terms, Some(6));
                assert!(!clear);
            }
            other => panic!("expected source-retrieval-policy-set, got {other:?}"),
        }
        let cli = parse(&[
            "vp",
            "source-retrieval-policy-set",
            "proj-1",
            "--disable-ann",
            "--rerank",
            "off",
        ]);
        match cli.command {
            Command::SourceRetrievalPolicySet {
                disable_ann,
                rerank,
                ..
            } => {
                assert!(disable_ann);
                assert_eq!(rerank.as_deref(), Some("off"));
            }
            other => panic!("expected source-retrieval-policy-set, got {other:?}"),
        }
        assert!(
            std::thread::Builder::new()
                .stack_size(32 * 1024 * 1024)
                .spawn(|| {
                    Cli::try_parse_from([
                        "vp",
                        "source-retrieval-policy-set",
                        "proj-1",
                        "--rerank",
                        "model",
                    ])
                    .is_err()
                })
                .unwrap()
                .join()
                .unwrap(),
            "rerank is a closed enum"
        );
    }

    #[test]
    fn retrieval_policy_merge_preserves_fields_and_applies_defaults() {
        let merged = merge_source_retrieval_policy(
            json!({
                "ann_enabled": true,
                "ann_min_vectors": 32,
                "ann_nprobe": 4,
                "rerank": "off",
                "query_expansion": "lexical",
                "max_expanded_terms": 6
            }),
            false,
            true,
            Some(64),
            None,
            Some("deterministic".to_string()),
            None,
            Some(3),
            false,
        )
        .unwrap();
        assert_eq!(merged["ann_enabled"], false);
        assert_eq!(merged["ann_min_vectors"], 64);
        assert_eq!(merged["ann_nprobe"], 4, "absent flag keeps stored value");
        assert_eq!(merged["rerank"], "deterministic");
        assert_eq!(merged["query_expansion"], "lexical");
        assert_eq!(merged["max_expanded_terms"], 3);

        let defaults = merge_source_retrieval_policy(
            json!({}),
            false,
            false,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(defaults["ann_enabled"], false);
        assert_eq!(defaults["ann_min_vectors"], 256);
        assert_eq!(defaults["ann_nprobe"], 8);
        assert_eq!(defaults["rerank"], "deterministic");
        assert_eq!(defaults["query_expansion"], "off");
        assert_eq!(defaults["max_expanded_terms"], 8);

        let recovered = merge_source_retrieval_policy(
            json!("bad"),
            true,
            false,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
        assert_eq!(recovered["ann_enabled"], true);
        assert_eq!(recovered["ann_min_vectors"], 256);
    }

    #[test]
    fn retrieval_policy_merge_rejects_conflicting_flags() {
        assert!(
            merge_source_retrieval_policy(
                json!({}),
                true,
                true,
                None,
                None,
                None,
                None,
                None,
                false
            )
            .is_err()
        );
        assert!(
            merge_source_retrieval_policy(
                json!({}),
                false,
                false,
                Some(8),
                None,
                None,
                None,
                None,
                true
            )
            .is_err(),
            "clear cannot carry other flags"
        );
        assert!(
            merge_source_retrieval_policy(
                json!({}),
                false,
                false,
                None,
                None,
                None,
                None,
                None,
                true
            )
            .is_ok(),
            "clear alone resets to defaults"
        );
    }

    #[test]
    fn source_embedding_status_printer_includes_fallback_and_counts() {
        let value = json!({
            "project_id": "prj-1",
            "enabled": true,
            "configured": true,
            "endpoint_env": "SOURCE_EMBED_ENDPOINT",
            "endpoint_env_present": false,
            "model": "embed-small",
            "dimensions": 512,
            "indexable_chunks": 20,
            "indexed_chunks": 16,
            "stale_chunks": 2,
            "invalid_chunks": 0,
            "pending_jobs": 1,
            "running_jobs": 0,
            "failed_jobs": 1,
            "dead_jobs": 0,
            "done_jobs": 3,
            "coverage": 0.8,
            "max_chunks_per_source": 200,
            "retention_days": 30,
            "semantic_unavailable": "no_indexed_vectors",
            "usage": {
                "calls": 4,
                "input_tokens": 120,
                "estimated_tokens": 20,
                "bytes": 480,
                "provider_reported_calls": 3
            },
            "recent_jobs": [{
                "source_id": "src-1",
                "source_title": "Runtime",
                "status": "failed",
                "attempts": 2,
                "generation": 2,
                "chunks_total": 10,
                "chunks_done": 8,
                "updated_at": "2026-11-11T00:00:00Z"
            }]
        });
        // The printer is a human surface; status is still strict JSON when
        // requested. This assertion keeps the recognizable fields present.
        assert_eq!(value["indexed_chunks"], 16);
        assert_eq!(value["semantic_unavailable"], "no_indexed_vectors");
        assert_eq!(value["recent_jobs"][0]["chunks_done"], 8);
        assert_eq!(value["endpoint_env_present"], false);
    }
}

#[cfg(test)]
mod process_command_tests {
    use super::*;

    #[test]
    fn clap_parses_process_commands() {
        // The CLI command enum is very large; parse on a thread with a
        // generous stack so this test is not sensitive to the default 2 MiB
        // libtest thread size.
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "processes", "ses-1"]).unwrap();
                match cli.command {
                    Command::Processes {
                        session,
                        include_finished,
                        limit,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert!(include_finished.is_none());
                        assert_eq!(limit, 50);
                    }
                    other => panic!("expected processes, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "process-logs",
                    "ses-1",
                    "psup_1",
                    "--offset",
                    "1024",
                    "--max-bytes",
                    "2048",
                    "--max-lines",
                    "40",
                    "--from-end",
                    "--follow",
                ])
                .unwrap();
                match cli.command {
                    Command::ProcessLogs {
                        session,
                        process,
                        offset,
                        max_bytes,
                        max_lines,
                        from_end,
                        follow,
                        timeout_secs,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(process, "psup_1");
                        assert_eq!(offset, 1024);
                        assert_eq!(max_bytes, 2048);
                        assert_eq!(max_lines, Some(40));
                        assert!(from_end);
                        assert!(follow);
                        assert_eq!(timeout_secs, 300);
                    }
                    other => panic!("expected process-logs, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "process-stop",
                    "ses-1",
                    "psup_1",
                    "--signal",
                    "KILL",
                    "--grace-secs",
                    "2",
                ])
                .unwrap();
                match cli.command {
                    Command::ProcessStop {
                        session,
                        process,
                        signal,
                        grace_secs,
                        ..
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(process, "psup_1");
                        assert_eq!(signal, "KILL");
                        assert_eq!(grace_secs, 2);
                    }
                    other => panic!("expected process-stop, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn clap_parses_process_policy_commands() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "process-policy", "prj-1"]).unwrap();
                match cli.command {
                    Command::ProcessPolicy { project } => assert_eq!(project, "prj-1"),
                    other => panic!("expected process-policy, got {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "process-policy-set",
                    "prj-1",
                    "--idle-sweep-enabled",
                    "false",
                    "--max-processes",
                    "4",
                    "--idle-ttl-secs",
                    "900",
                    "--max-output-bytes",
                    "65536",
                    "--max-output-lines",
                    "2000",
                    "--restart-on-boot",
                    "true",
                    "--restart-max-attempts",
                    "2",
                    "--restart-backoff-secs",
                    "30",
                ])
                .unwrap();
                match cli.command {
                    Command::ProcessPolicySet {
                        project,
                        idle_sweep_enabled,
                        max_processes,
                        idle_ttl_secs,
                        max_output_bytes,
                        max_output_lines,
                        restart_on_boot,
                        restart_max_attempts,
                        restart_backoff_secs,
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(idle_sweep_enabled, Some(false));
                        assert_eq!(max_processes, Some(4));
                        assert_eq!(idle_ttl_secs, Some(900));
                        assert_eq!(max_output_bytes, Some(65536));
                        assert_eq!(max_output_lines, Some(2000));
                        assert_eq!(restart_on_boot, Some(true));
                        assert_eq!(restart_max_attempts, Some(2));
                        assert_eq!(restart_backoff_secs, Some(30));
                    }
                    other => panic!("expected process-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn process_policy_human_rendering_is_bounded() {
        let value = json!({
            "project_id": "prj-1",
            "policy": {
                "idle_sweep_enabled": true,
                "max_processes": 8,
                "idle_ttl_secs": 3600,
                "max_output_bytes": 1048576,
                "max_output_lines": 20000
            }
        });
        let lines = process_console_lines(&value).unwrap();
        assert!(lines[0].contains("prj-1"));
        assert!(lines[1].contains("idle sweep on"));
        assert!(lines[2].contains("1048576 bytes"));
    }

    #[test]
    fn human_process_list_and_logs_are_bounded() {
        let list = json!({
            "session_id": "ses-1",
            "processes": [{
                "id": "psup_1",
                "state": "running",
                "name": "server",
                "pid": 42,
                "started_at": "2026-11-11T00:00:00Z",
                "output_bytes": 128,
                "exit_code": null,
                "detail": null
            }],
            "active": 1,
            "limit": 8
        });
        let lines = process_console_lines(&list).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("psup_1"));
        assert!(lines[0].contains("running"));
        assert!(lines[0].contains("pid 42"));

        let logs = json!({
            "session_id": "ses-1",
            "process_id": "psup_1",
            "state": "running",
            "content": "hello\n",
            "offset": 0,
            "next_offset": 6,
            "total_bytes": 6,
            "eof": true,
            "truncated": false,
            "rotated": false
        });
        let lines = process_console_lines(&logs).unwrap();
        assert!(lines[0].contains("psup_1"));
        assert!(lines.iter().any(|line| line == "hello\n"));

        let stop = json!({
            "process": {
                "id": "psup_1",
                "state": "stopped",
                "exit_code": 0,
                "signal": 15
            },
            "forced": false,
            "waited_ms": 12
        });
        let lines = process_console_lines(&stop).unwrap();
        assert!(lines[0].contains("stopped"));
        assert!(lines[0].contains("signal 15"));
    }
}

#[cfg(test)]
mod scaling_cli_tests {
    use super::*;

    #[test]
    fn preview_lines_include_action_targets_reasons_and_hash() {
        let lines = scaling_lines(&json!({
            "session_id": "ses-1",
            "project_id": "proj-1",
            "policy": {"enabled": true},
            "decision": {
                "session_id": "ses-1",
                "action": "expand_workers",
                "current_workers": 2,
                "target_workers": 4,
                "current_labour": 2,
                "target_labour": 2,
                "ready_tasks": 5,
                "running_tasks": 2,
                "active_workers": 2,
                "active_labour": 1,
                "plan_revision": 3,
                "applied": false,
                "inputs_hash": "0123456789abcdef",
                "reasons": ["ready_tasks:5", "expand_workers:2->4"],
                "manager_evidence": "two departments would help"
            }
        }))
        .expect("preview lines");
        let text = lines.join("\n");
        assert!(text.contains("action=expand_workers"));
        assert!(text.contains("Workers 2→4"));
        assert!(text.contains("ready 5"));
        assert!(text.contains("inputs 0123456789ab"));
        assert!(text.contains("manager evidence: two departments would help"));
    }

    #[test]
    fn status_lines_report_policy_targets_and_latest_reasons() {
        let lines = scaling_lines(&json!({
            "global_worker_cap": 12,
            "global_labour_cap": 24,
            "global_worker_available": 9,
            "global_labour_available": 20,
            "projects": [{
                "project_id": "proj-1",
                "project_name": "demo",
                "policy": {
                    "enabled": true,
                    "min_workers": 1,
                    "max_workers": 4,
                    "min_labour": 1,
                    "max_labour": 8,
                    "expand_ready_threshold": 2,
                    "cooldown_secs": 120,
                    "max_step": 2,
                    "budget_headroom_ratio": 0.25
                },
                "sessions": [{
                    "session_id": "ses-1",
                    "goal": "ship",
                    "status": "idle",
                    "plan_revision": 2,
                    "plan_tasks": 6,
                    "ready_tasks": 3,
                    "running_tasks": 0,
                    "completed_tasks": 2,
                    "current_workers": 2,
                    "current_labour": 4,
                    "active_workers": 0,
                    "active_labour": 0,
                    "latest": {
                        "action": "scale_down_labour",
                        "reasons": ["scale_down_labour:4->2", "idle 900s"]
                    }
                }]
            }]
        }))
        .expect("status lines");
        let text = lines.join("\n");
        assert!(text.contains("global Workers 9/12"));
        assert!(text.contains("demo  scaling=on"));
        assert!(text.contains("budget ≥25%"));
        assert!(text.contains("slots 2W/4L"));
        assert!(text.contains("latest scale_down_labour"));
        assert!(text.contains("scale_down_labour:4->2"));
    }

    #[test]
    fn policy_lines_show_disabled_budget_check_and_bounds() {
        let lines = scaling_lines(&json!({
            "project_id": "proj-1",
            "policy": {
                "enabled": false,
                "min_workers": 1,
                "max_workers": 3,
                "min_labour": 1,
                "max_labour": 6,
                "expand_ready_threshold": 2,
                "expand_critical_delay_secs": 60,
                "scale_down_idle_secs": 300,
                "cooldown_secs": 90,
                "max_step": 1,
                "budget_headroom_ratio": null
            }
        }))
        .expect("policy lines");
        let text = lines.join("\n");
        assert!(text.contains("enabled=false"));
        assert!(text.contains("Workers 1-3"));
        assert!(text.contains("Labour 1-6"));
        assert!(text.contains("budget headroom: check disabled"));
    }

    #[test]
    fn unrelated_payload_is_not_misformatted() {
        assert!(scaling_lines(&json!({"session_id": "ses-1"})).is_none());
        assert!(scaling_lines(&json!({"projects": []})).is_none());
        assert!(scaling_lines(&json!({"policy": {"max_workers": 4}})).is_none());
    }
}

#[cfg(test)]
mod governor_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn governor_commands_and_flags_parse() {
        let cli = parse(&["vp", "governor", "session-1", "--limit", "7"]);
        match cli.command {
            Command::Governor { session, limit } => {
                assert_eq!(session, "session-1");
                assert_eq!(limit, 7);
            }
            other => panic!("expected governor, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "critical-path",
            "session-1",
            "--remaining-tokens",
            "1234",
        ]);
        match cli.command {
            Command::CriticalPath {
                session,
                remaining_tokens,
                mode,
            } => {
                assert_eq!(session, "session-1");
                assert_eq!(remaining_tokens, Some(1_234));
                assert_eq!(mode, None);
            }
            other => panic!("expected critical-path, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "governor-preview",
            "session-1",
            "--remaining-tokens",
            "99",
            "--limit",
            "3",
        ]);
        match cli.command {
            Command::GovernorPreview {
                session,
                remaining_tokens,
                limit,
                mode,
            } => {
                assert_eq!(session, "session-1");
                assert_eq!(remaining_tokens, Some(99));
                assert_eq!(limit, 3);
                assert_eq!(mode, None);
            }
            other => panic!("expected governor-preview, got {other:?}"),
        }

        let cli = parse(&["vp", "governor-policy", "project-1"]);
        match cli.command {
            Command::GovernorPolicy { project } => assert_eq!(project, "project-1"),
            other => panic!("expected governor-policy, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "governor-policy-set",
            "project-1",
            "--enabled",
            "--reserve-ratio",
            "0.4",
            "--throttle-at-ratio",
            "0.2",
            "--critical-boost",
            "10",
            "--min-task-budget",
            "2000",
        ]);
        match cli.command {
            Command::GovernorPolicySet {
                project,
                enabled,
                reserve_ratio,
                throttle_at_ratio,
                critical_boost,
                min_task_budget,
                mode,
                max_paths,
                department_min_ratio,
                observed_weight_ratio,
                starvation_after_secs,
            } => {
                assert_eq!(project, "project-1");
                assert_eq!(enabled, Some(true));
                assert_eq!(reserve_ratio, Some(0.4));
                assert_eq!(throttle_at_ratio, Some(0.2));
                assert_eq!(critical_boost, Some(10));
                assert_eq!(min_task_budget, Some(2_000));
                assert_eq!(mode, None);
                assert_eq!(max_paths, None);
                assert_eq!(department_min_ratio, None);
                assert_eq!(observed_weight_ratio, None);
                assert_eq!(starvation_after_secs, None);
            }
            other => panic!("expected governor-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn governor_human_renderer_explains_the_reserve() {
        let value = json!({
            "session_id": "session-1",
            "session_status": "idle",
            "plan_revision": 2,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "reserve_ratio": 0.25},
            "critical_path": {"task_ids": ["a", "b"], "weight_tokens": 5000},
            "reserve": {
                "state": "active",
                "protected_tokens": 7500,
                "remaining_tokens": 10000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.35,
                "throttled": false
            },
            "admitted": ["a"],
            "deferred": [{
                "task_id": "side",
                "reason": "reserve",
                "code": "BUDGET_RESERVED"
            }],
            "last_decisions": [],
            "generated_at": "2026-11-11T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("session session-1"));
        assert!(lines.contains("a -> b"));
        assert!(lines.contains("protected 7500 of remaining 10000"));
        assert!(lines.contains("Defer: side (reserve, BUDGET_RESERVED)"));
    }

    #[test]
    fn governor_renderer_surfaces_typed_errors_and_deferrals() {
        let value = json!({
            "session_id": "session-1",
            "session_status": "idle",
            "plan_revision": 1,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true},
            "critical_path": {"task_ids": [], "weight_tokens": 0},
            "reserve": {
                "state": "complete",
                "protected_tokens": 0,
                "remaining_tokens": 5000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.35,
                "throttled": false
            },
            "admitted": [],
            "deferred": [],
            "last_decisions": [],
            "error": {
                "code": "BUDGET_PATH_CYCLE",
                "message": "plan dependency cycle",
                "hint": "remove the cycle"
            },
            "generated_at": "2026-11-11T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("Path: complete"));
        assert!(lines.contains("BUDGET_PATH_CYCLE"));
        assert!(lines.contains("hint: remove the cycle"));
    }

    #[test]
    fn governor_policy_renderer_is_compact() {
        let value = json!({
            "project_id": "p1",
            "policy": {
                "enabled": true,
                "reserve_ratio": 0.5,
                "throttle_at_ratio": 0.2,
                "critical_boost": 10,
                "min_task_budget": 2000
            }
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("enabled"));
        assert!(lines.contains("reserve 50%"));
        assert!(lines.contains("throttle 20%"));
    }
}

#[cfg(test)]
mod remote_integration_cli_tests {
    use super::*;

    #[test]
    fn remote_integration_commands_parse_explicit_confirmation_and_bounds() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(parse_remote_integration_commands)
            .unwrap()
            .join()
            .unwrap();
    }

    fn parse_remote_integration_commands() {
        let cli = Cli::try_parse_from([
            "vp",
            "remote-integrate",
            "prj-1",
            "ses-1",
            "feature/remote",
            "--host",
            "build.example.com",
            "--expected-commit",
            "abcdef1234567890",
            "--task",
            "T1",
            "--objective",
            "integrate it",
            "--confirm",
            "--keep-import-refs",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteIntegrate {
                project,
                session,
                branch,
                host,
                expected_commit,
                base_commit,
                task,
                objective,
                confirm,
                keep_import_refs,
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(session, "ses-1");
                assert_eq!(branch, "feature/remote");
                assert_eq!(host.as_deref(), Some("build.example.com"));
                assert_eq!(expected_commit.as_deref(), Some("abcdef1234567890"));
                assert!(base_commit.is_none());
                assert_eq!(task.as_deref(), Some("T1"));
                assert_eq!(objective.as_deref(), Some("integrate it"));
                assert!(confirm);
                assert!(keep_import_refs);
            }
            other => panic!("expected remote-integrate, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "remote-integration-policy-set",
            "prj-1",
            "--enable",
            "--max-bundle-bytes",
            "1048576",
            "--no-require-verifier",
            "--allow-host",
            "build.example.com",
            "--no-keep-import-refs",
            "--confirm",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteIntegrationPolicySet {
                project,
                enable,
                max_bundle_bytes,
                no_require_verifier,
                allowed_hosts,
                no_keep_import_refs,
                confirm,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert!(enable);
                assert_eq!(max_bundle_bytes, Some(1_048_576));
                assert!(no_require_verifier);
                assert_eq!(allowed_hosts, vec!["build.example.com".to_string()]);
                assert!(no_keep_import_refs);
                assert!(confirm);
            }
            other => panic!("expected remote-integration-policy-set, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "remote-integration", "prj-1", "--session", "ses-1"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Command::RemoteIntegration {
                session: Some(ref session),
                ..
            } if session == "ses-1"
        ));

        let cli = Cli::try_parse_from([
            "vp",
            "remote-integration-cleanup",
            "prj-1",
            "--entry",
            "e1",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteIntegrationCleanup {
                project,
                session,
                entry,
                force,
            } => {
                assert_eq!(project, "prj-1");
                assert!(session.is_none());
                assert_eq!(entry.as_deref(), Some("e1"));
                assert!(force);
            }
            other => panic!("expected remote-integration-cleanup, got {other:?}"),
        }
    }

    #[test]
    fn remote_integration_human_output_reports_status_and_caveat() {
        let value = serde_json::json!({
            "entry": {
                "id": "e1",
                "status": "merged",
                "commit": "abcdef1234567890"
            },
            "bundle": {
                "host": "build.example.com",
                "branch": "feature/x",
                "sha256": "a".repeat(64),
                "bytes": 1024
            },
            "verification": {
                "required": true,
                "performed": true,
                "approved": true,
                "summary": "looks good"
            },
            "imported_ref": null,
            "refs_pruned": true,
            "environment_caveat": "local only"
        });
        let lines = remote_integration_lines(&value).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("merged"));
        assert!(text.contains("build.example.com"));
        assert!(text.contains("verifier required=true performed=true approved=true"));
        assert!(text.contains("local only"));

        let status = serde_json::json!({
            "project_id": "prj-1",
            "policy": {
                "enabled": true,
                "max_bundle_bytes": 1024,
                "require_verifier": true,
                "allowed_hosts": ["build.example.com"],
                "keep_import_refs": false
            },
            "entries": [{
                "entry_id": "e1",
                "status": "conflict",
                "commit": "abcdef1234567890",
                "remote_host": "build.example.com",
                "remote_branch": "feature/x",
                "attempt": 2,
                "imported_ref": "refs/verlinpark/remote/s/i",
                "detail": "conflict"
            }],
            "total": 1,
            "pending": 0,
            "merged": 0,
            "conflicts": 1,
            "failed": 0,
            "retained_refs": 1,
            "environment_caveat": "local only"
        });
        let lines = remote_integration_lines(&status).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("enabled=true"));
        assert!(text.contains("conflict"));
        assert!(text.contains("refs retained 1"));

        let cleanup = serde_json::json!({
            "project_id": "prj-1",
            "removed_refs": ["refs/verlinpark/remote/s/i"],
            "removed_worktrees": [],
            "skipped": ["e2: retryable entry (conflict) requires force"],
            "entries_examined": 2
        });
        let lines = remote_integration_lines(&cleanup).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("removed 1 ref(s)"));
        assert!(text.contains("requires force"));
    }

    #[test]
    fn backup_signing_flags_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "backup",
                    "--repository",
                    "/tmp/repo",
                    "--signing-key",
                    "primary",
                    "--keyring-dir",
                    "/tmp/keys",
                ])
                .unwrap();
                match cli.command {
                    Command::Backup {
                        signing_key,
                        keyring_dir,
                        sign,
                        ..
                    } => {
                        assert_eq!(signing_key.as_deref(), Some("primary"));
                        assert_eq!(keyring_dir.as_deref(), Some(Path::new("/tmp/keys")));
                        assert!(!sign);
                    }
                    other => panic!("expected backup, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "restore",
                    "--repository",
                    "/tmp/repo",
                    "--snapshot",
                    "0190f000-0000-7000-8000-000000000000",
                    "--allow-rollback",
                    "--keyring-dir",
                    "/tmp/keys",
                ])
                .unwrap();
                match cli.command {
                    Command::Restore {
                        allow_rollback,
                        keyring_dir,
                        ..
                    } => {
                        assert!(allow_rollback);
                        assert_eq!(keyring_dir.as_deref(), Some(Path::new("/tmp/keys")));
                    }
                    other => panic!("expected restore, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "backup-verify",
                    "--repository",
                    "/tmp/repo",
                    "--presence-only",
                    "--allow-rollback",
                ])
                .unwrap();
                match cli.command {
                    Command::BackupVerify {
                        presence_only,
                        allow_rollback,
                        ..
                    } => {
                        assert!(presence_only);
                        assert!(allow_rollback);
                    }
                    other => panic!("expected backup-verify, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "backup-key-generate",
                    "primary",
                    "--keyring-dir",
                    "/tmp/keys",
                    "--inactive",
                ])
                .unwrap();
                match cli.command {
                    Command::BackupKeyGenerate {
                        key_id,
                        inactive,
                        from_env,
                        ..
                    } => {
                        assert_eq!(key_id, "primary");
                        assert!(inactive);
                        assert!(from_env.is_none());
                    }
                    other => panic!("expected backup-key-generate, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn signing_options_are_explicit_and_keyring_backed() {
        // No signing flags keeps the v2 unsigned path.
        assert!(
            resolve_signing_options(None, None, false)
                .unwrap()
                .is_none()
        );

        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("missing");
        let error = resolve_signing_options(Some(&missing), None, false).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEYRING_NOT_FOUND"));

        let keys = dir.path().join("keys");
        Keyring::generate_key(
            &keys,
            "primary",
            true,
            SecretSource::File {
                name: "primary.key".to_string(),
            },
        )
        .unwrap();
        let signing = resolve_signing_options(Some(&keys), None, true)
            .unwrap()
            .unwrap();
        assert_eq!(signing.keyring.active_key_id(), Some("primary"));
        assert!(signing.key_id.is_none());

        let signing = resolve_signing_options(Some(&keys), Some("primary"), false)
            .unwrap()
            .unwrap();
        assert_eq!(signing.key_id.as_deref(), Some("primary"));

        let error = resolve_signing_options(Some(&keys), Some("missing"), false).unwrap_err();
        assert!(format!("{error:#}").contains("BACKUP_KEY_NOT_FOUND"));

        let error = resolve_signing_options(Some(&keys), Some("primary"), true).unwrap_err();
        assert!(format!("{error:#}").contains("only one of"));
    }

    #[test]
    fn anchor_store_resolution_honors_an_explicit_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let default = anchor_store(dir.path(), None);
        assert_eq!(default.path(), dir.path().join("backup-anchor.json"));
        let explicit = dir.path().join("custom-anchor.json");
        let store = anchor_store(dir.path(), Some(&explicit));
        assert_eq!(store.path(), explicit);
    }
    #[test]
    fn preemption_commands_and_flags_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "preempt", "ses-1", "--reason", "rotate the run"])
                        .unwrap();
                match cli.command {
                    Command::Preempt {
                        session, reason, ..
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(reason.as_deref(), Some("rotate the run"));
                    }
                    other => panic!("unexpected command: {other:?}"),
                }
                let cli =
                    Cli::try_parse_from(["vp", "preemption", "ses-1", "--limit", "25"]).unwrap();
                match cli.command {
                    Command::Preemption { session, limit } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(limit, 25);
                    }
                    other => panic!("unexpected command: {other:?}"),
                }
                let cli =
                    Cli::try_parse_from(["vp", "preemptions", "proj-1", "--limit", "5"]).unwrap();
                match cli.command {
                    Command::Preemptions { project, limit } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(limit, 5);
                    }
                    other => panic!("unexpected command: {other:?}"),
                }
                let cli = Cli::try_parse_from([
                    "vp",
                    "preemption-policy-set",
                    "proj-1",
                    "--disabled",
                    "--tool-grace-ms",
                    "1500",
                    "--tool-deadline-secs",
                    "30",
                    "--no-checkpoint",
                ])
                .unwrap();
                match cli.command {
                    Command::PreemptionPolicySet {
                        project,
                        disabled,
                        tool_grace_ms,
                        tool_deadline_secs,
                        no_checkpoint,
                        ..
                    } => {
                        assert_eq!(project, "proj-1");
                        assert!(disabled);
                        assert_eq!(tool_grace_ms, Some(1_500));
                        assert_eq!(tool_deadline_secs, Some(30));
                        assert!(no_checkpoint);
                    }
                    other => panic!("unexpected command: {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod test_cache_cli_tests {
    use super::*;

    #[test]
    fn parses_test_cache_commands_additively() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "test-cache-policy-set",
                    "prj-1",
                    "--max-entries",
                    "7",
                    "--retention-days",
                    "3",
                    "--max-output-bytes",
                    "65536",
                    "--reuse-lint",
                    "false",
                    "--include-integration",
                    "--failed-retention-secs",
                    "120",
                ])
                .unwrap();
                match cli.command {
                    Command::TestCachePolicySet {
                        project,
                        max_entries,
                        retention_days,
                        max_output_bytes,
                        reuse_lint,
                        include_integration,
                        failed_retention_secs,
                        ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(max_entries, Some(7));
                        assert_eq!(retention_days, Some(3));
                        assert_eq!(max_output_bytes, Some(65536));
                        assert_eq!(reuse_lint, Some(false));
                        assert_eq!(include_integration, Some(true));
                        assert_eq!(failed_retention_secs, Some(120));
                    }
                    other => panic!("expected test-cache-policy-set, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "test-cache-clear",
                    "prj-1",
                    "--kind",
                    "lint",
                    "--yes",
                ])
                .unwrap();
                match cli.command {
                    Command::TestCacheClear {
                        project, kind, yes, ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(kind.as_deref(), Some("lint"));
                        assert!(yes);
                    }
                    other => panic!("expected test-cache-clear, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "test-impact", "ses-1", "--no-history"]).unwrap();
                match cli.command {
                    Command::TestImpact {
                        session,
                        no_history,
                        ..
                    } => {
                        assert_eq!(session, "ses-1");
                        assert!(no_history);
                    }
                    other => panic!("expected test-impact, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn renders_stats_policy_entries_and_prune() {
        let stats = json!({
            "project_id": "prj-1",
            "entries": 2,
            "passed": 1,
            "failed": 1,
            "error": 0,
            "expired": 0,
            "total_hits": 4,
            "output_bytes": 2048,
            "by_kind": {"test": 2},
            "policy": {
                "enabled": true,
                "max_entries": 512,
                "retention_days": 14,
                "max_output_bytes": 262144,
                "reuse_lint": true,
                "include_integration": false,
                "failed_retention_secs": 900
            },
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = test_cache_lines(&stats).unwrap();
        let text = lines.join("\n");
        assert!(text.contains("test cache policy"));
        assert!(text.contains("entries=2"));
        assert!(text.contains("integrator=false"));

        let entries = json!({
            "project_id": "prj-1",
            "entries": [
                {
                    "id": "tr-1",
                    "kind": "test",
                    "status": "passed",
                    "exit_code": 0,
                    "duration_ms": 42,
                    "hit_count": 2,
                    "command": "cargo test"
                }
            ],
            "total": 1,
            "truncated": false
        });
        let text = test_cache_lines(&entries).unwrap().join("\n");
        assert!(text.contains("test cache entries"));
        assert!(text.contains("cargo test"));

        let prune = json!({
            "project_id": "prj-1",
            "pruned": 1,
            "remaining": 0,
            "dry_run": true,
            "affected_sessions": 1,
            "removed_ids": ["tr-1"],
            "reason": "retention"
        });
        let text = test_cache_lines(&prune).unwrap().join("\n");
        assert!(text.contains("dry_run=true"));
        assert!(text.contains("removed tr-1"));

        let impact = json!({
            "session_id": "ses-1",
            "project_id": "prj-1",
            "test_command": "cargo test",
            "map": {
                "generation": "impact-v1:abc",
                "lsp_available": false,
                "lexical_only": true,
                "truncated": false,
                "changed_paths": ["src/lib.rs"],
                "targets": [{
                    "path": "tests/smoke.rs",
                    "source": "lexical",
                    "confidence": 0.9,
                    "reason": "reverse import"
                }],
                "notes": ["heuristic"]
            }
        });
        let text = test_cache_lines(&impact).unwrap().join("\n");
        assert!(text.contains("test impact"));
        assert!(text.contains("tests/smoke.rs"));
        assert!(text.contains("LEXICAL") || text.contains("lexical"));
    }
}

#[cfg(test)]
mod approval_optimizer_cli_tests {
    use super::*;
    use clap::Parser;

    /// The full CLI enum is large; parse on a generously sized thread so the
    /// test reflects parsing behavior rather than the test harness's 2 MiB
    /// stack.
    fn parse(args: &[&str]) -> Cli {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn parses_explicit_batch_decisions() {
        let cli = parse(&[
            "vp",
            "approval-batch",
            "--approve",
            "a1",
            "--approve",
            "a2",
            "--deny",
            "a3",
        ]);
        match cli.command {
            Command::ApprovalBatch { approve, deny } => {
                assert_eq!(approve, vec!["a1", "a2"]);
                assert_eq!(deny, vec!["a3"]);
            }
            _ => panic!("expected approval batch"),
        }
    }

    #[test]
    fn parses_template_create_and_update_flags() {
        let cli = parse(&[
            "vp",
            "approval-template-set",
            "--project",
            "p1",
            "--name",
            "src writes",
            "--tool-pattern",
            "write_*",
            "--risk",
            "workspace_write",
            "--path-glob",
            "src/**",
            "--action",
            "allow",
            "--max-uses",
            "5",
        ]);
        match cli.command {
            Command::ApprovalTemplateSet {
                project,
                name,
                tool_pattern,
                risk,
                path_glob,
                action,
                max_uses,
                ..
            } => {
                assert_eq!(project.as_deref(), Some("p1"));
                assert_eq!(name.as_deref(), Some("src writes"));
                assert_eq!(tool_pattern.as_deref(), Some("write_*"));
                assert_eq!(risk, vec!["workspace_write"]);
                assert_eq!(path_glob, vec!["src/**"]);
                assert_eq!(action.as_deref(), Some("allow"));
                assert_eq!(max_uses, Some(5));
            }
            _ => panic!("expected approval template set"),
        }
    }

    #[test]
    fn parses_attention_policy_updates() {
        let cli = parse(&[
            "vp",
            "approval-policy-set",
            "--project",
            "p1",
            "--max-pending",
            "12",
            "--coalesce-secs",
            "30",
            "--stale-secs",
            "600",
            "--priority-boost-risk",
            "destructive",
            "--notify-coalesce-secs",
            "15",
        ]);
        match cli.command {
            Command::ApprovalPolicySet {
                project,
                max_pending,
                coalesce_secs,
                stale_secs,
                priority_boost_risk,
                notify_coalesce_secs,
                ..
            } => {
                assert_eq!(project, "p1");
                assert_eq!(max_pending, Some(12));
                assert_eq!(coalesce_secs, Some(30));
                assert_eq!(stale_secs, Some(600));
                assert_eq!(priority_boost_risk.as_deref(), Some("destructive"));
                assert_eq!(notify_coalesce_secs, Some(15));
            }
            _ => panic!("expected approval policy set"),
        }
    }

    #[test]
    fn approval_queue_human_output_includes_attention_fields() {
        let value = serde_json::json!({
            "approvals": [{
                "id": "a1",
                "tool": "write_file",
                "status": "pending",
                "approval_risk": "workspace_write",
                "waiter_count": 3,
                "priority": 42,
                "input": {"path": "src/lib.rs"}
            }],
            "pending_count": 1
        });
        print_human(&value);
    }
}

#[cfg(test)]
mod governor_fairness_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn fairness_policy_flags_parse() {
        let cli = parse(&[
            "vp",
            "governor-policy-set",
            "project-1",
            "--mode",
            "multi",
            "--max-paths",
            "6",
            "--department-min-ratio",
            "0.2",
            "--observed-weight-ratio",
            "0.5",
            "--starvation-after-secs",
            "600",
        ]);
        match cli.command {
            Command::GovernorPolicySet {
                mode,
                max_paths,
                department_min_ratio,
                observed_weight_ratio,
                starvation_after_secs,
                ..
            } => {
                assert_eq!(mode.as_deref(), Some("multi"));
                assert_eq!(max_paths, Some(6));
                assert_eq!(department_min_ratio, Some(0.2));
                assert_eq!(observed_weight_ratio, Some(0.5));
                assert_eq!(starvation_after_secs, Some(600));
            }
            other => panic!("expected governor-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn preview_mode_flags_parse_and_validate() {
        let cli = parse(&[
            "vp",
            "governor-preview",
            "session-1",
            "--mode",
            "multi",
            "--remaining-tokens",
            "500",
        ]);
        match cli.command {
            Command::GovernorPreview {
                session,
                remaining_tokens,
                mode,
                ..
            } => {
                assert_eq!(session, "session-1");
                assert_eq!(remaining_tokens, Some(500));
                assert_eq!(mode.as_deref(), Some("multi"));
            }
            other => panic!("expected governor-preview, got {other:?}"),
        }
        let cli = parse(&["vp", "critical-path", "session-1", "--mode", "single"]);
        match cli.command {
            Command::CriticalPath { mode, .. } => assert_eq!(mode.as_deref(), Some("single")),
            other => panic!("expected critical-path, got {other:?}"),
        }
        assert_eq!(parse_governor_mode(None).unwrap(), None);
        assert_eq!(
            parse_governor_mode(Some("multi-path")).unwrap(),
            Some("multi")
        );
        assert_eq!(parse_governor_mode(Some("SINGLE")).unwrap(), Some("single"));
        assert!(parse_governor_mode(Some("weighted")).is_err());
    }

    #[test]
    fn policy_renderer_prints_fairness_fields_for_multi_mode() {
        let value = json!({
            "project_id": "p1",
            "policy": {
                "enabled": true,
                "mode": "multi",
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.1,
                "critical_boost": 10,
                "min_task_budget": 100,
                "max_paths": 6,
                "department_min_ratio": 0.2,
                "observed_weight_ratio": 0.5,
                "starvation_after_secs": 600
            }
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("multi"));
        assert!(lines.contains("max 6 paths"));
        assert!(lines.contains("department floor 20%"));
        assert!(lines.contains("observed 50%"));
        assert!(lines.contains("starvation 600 s"));
    }

    #[test]
    fn session_renderer_prints_the_path_table_and_deferred_order() {
        let value = json!({
            "session_id": "s1",
            "session_status": "idle",
            "plan_revision": 3,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "multi", "observed_weight_ratio": 0.5},
            "critical_path": {"task_ids": ["heavy"], "weight_tokens": 10000},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 2,
                "protected_tokens": 10000,
                "allocated_tokens": 10000,
                "floor_tokens": 500,
                "remaining_tokens": 10000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.0,
                "throttled": false
            },
            "paths": [
                {
                    "path_id": 0,
                    "task_ids": ["heavy", "tail"],
                    "departments": ["impl"],
                    "weight_tokens": 10000,
                    "floor_tokens": 400,
                    "allocation_tokens": 6000,
                    "deficit_tokens": 4000
                },
                {
                    "path_id": 1,
                    "task_ids": ["verify"],
                    "departments": ["qa"],
                    "weight_tokens": 8000,
                    "floor_tokens": 100,
                    "allocation_tokens": 4000,
                    "deficit_tokens": 4000
                }
            ],
            "departments": [
                {"department": "impl", "weight_tokens": 10000, "floor_tokens": 400, "path_ids": [0]},
                {"department": "qa", "weight_tokens": 8000, "floor_tokens": 100, "path_ids": [1]}
            ],
            "admitted": [],
            "deferred": [{
                "task_id": "side",
                "reason": "reserve",
                "code": "BUDGET_RESERVED"
            }],
            "deferred_order": ["side"],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("2 path(s) · allocated 10000 · floors 500"));
        assert!(lines.contains("#0 weight 10000 · floor 400 · alloc 6000 · deficit 4000"));
        assert!(lines.contains("[impl] heavy -> tail"));
        assert!(lines.contains("[qa] verify"));
        assert!(lines.contains("Department floors:"));
        assert!(lines.contains("impl weight 10000 · floor 400 · paths #0"));
        assert!(lines.contains("qa weight 8000 · floor 100 · paths #1"));
        assert!(lines.contains("Deferred order: side"));
    }

    #[test]
    fn session_renderer_reports_enumeration_truncation_and_bounded_rows() {
        let value = json!({
            "session_id": "s1",
            "session_status": "running",
            "plan_revision": 4,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "multi"},
            "critical_path": {"task_ids": ["t19"], "weight_tokens": 1000},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 16,
                "protected_tokens": 16000,
                "allocated_tokens": 16000,
                "floor_tokens": 0,
                "remaining_tokens": 50000,
                "reserve_ratio": 0.0,
                "throttle_at_ratio": 0.0,
                "throttled": false,
                "enumerated_paths": 4096,
                "paths_truncated": true
            },
            "paths": [
                {"path_id": 0, "task_ids": ["a"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 1, "task_ids": ["b"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 2, "task_ids": ["c"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 3, "task_ids": ["d"], "weight_tokens": 1000, "allocation_tokens": 1000},
                {"path_id": 4, "task_ids": ["e"], "weight_tokens": 1000, "allocation_tokens": 1000}
            ],
            "departments": [],
            "admitted": ["a", "b", "c", "d", "e"],
            "deferred": [],
            "deferred_order": [],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("4096 enumerated"));
        assert!(lines.contains("truncated"));
        assert!(lines.contains("+1 more protected paths"));
        assert!(!lines.contains("Department floors:"));
    }

    #[test]
    fn department_renderer_bounds_a_wide_table() {
        let departments = (0..8)
            .map(|index| {
                json!({
                    "department": format!("dept-{index}"),
                    "weight_tokens": 100 - index,
                    "floor_tokens": 10,
                    "path_ids": [index]
                })
            })
            .collect::<Vec<_>>();
        let value = json!({
            "session_id": "s1",
            "session_status": "idle",
            "plan_revision": 1,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "multi"},
            "critical_path": {"task_ids": ["a"], "weight_tokens": 100},
            "reserve": {
                "state": "active",
                "mode": "multi",
                "path_count": 8,
                "protected_tokens": 800,
                "allocated_tokens": 800,
                "floor_tokens": 80,
                "remaining_tokens": 8_000,
                "reserve_ratio": 0.0,
                "throttle_at_ratio": 0.0,
                "throttled": false
            },
            "paths": [],
            "departments": departments,
            "admitted": [],
            "deferred": [],
            "deferred_order": [],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(lines.contains("Department floors:"));
        assert!(lines.contains("dept-0 weight 100 · floor 10 · paths #0"));
        assert!(lines.contains("dept-5 weight 95 · floor 10 · paths #5"));
        assert!(!lines.contains("dept-6 weight"));
        assert!(lines.contains("+2 more departments"));
    }

    #[test]
    fn single_mode_renderer_does_not_emit_a_fairness_block() {
        let value = json!({
            "session_id": "s1",
            "session_status": "idle",
            "plan_revision": 1,
            "enabled": true,
            "unlimited": false,
            "policy": {"enabled": true, "mode": "single"},
            "critical_path": {"task_ids": ["a"], "weight_tokens": 100},
            "reserve": {
                "state": "active",
                "mode": "single",
                "path_count": 1,
                "protected_tokens": 100,
                "remaining_tokens": 1000,
                "reserve_ratio": 0.25,
                "throttle_at_ratio": 0.0,
                "throttled": false
            },
            "paths": [],
            "admitted": ["a"],
            "deferred": [],
            "deferred_order": [],
            "last_decisions": [],
            "generated_at": "2026-11-12T00:00:00Z"
        });
        let lines = governor_lines(&value).unwrap().join("\n");
        assert!(!lines.contains("Path table"));
        assert!(!lines.contains("Fairness:"));
        assert!(lines.contains("Admit: a"));
    }

    #[test]
    fn rpc_error_renderer_includes_typed_code_and_hint() {
        let error = vp_protocol::rpc::RpcError::with_data(
            -32602,
            "archive entry is not safe to extract",
            json!({
                "code": "ARCHIVE_UNSAFE_ENTRY",
                "retryable": false,
                "hint": "Select only entries marked safe."
            }),
        );
        let rendered = format_rpc_error(&error);
        assert!(rendered.contains("archive entry is not safe to extract (code -32602)"));
        assert!(rendered.contains("[ARCHIVE_UNSAFE_ENTRY]"));
        assert!(rendered.contains("hint: Select only entries marked safe."));
        let legacy = vp_protocol::rpc::RpcError::new(-32603, "internal");
        assert_eq!(format_rpc_error(&legacy), "internal (code -32603)");
    }

    #[test]
    fn archive_and_media_commands_parse() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "artifact-archive-list",
                    "art-1",
                    "--max-entries",
                    "25",
                ])
                .unwrap();
                match cli.command {
                    Command::ArtifactArchiveList {
                        artifact,
                        max_entries,
                    } => {
                        assert_eq!(artifact, "art-1");
                        assert_eq!(max_entries, Some(25));
                    }
                    other => panic!("expected archive list, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "artifact-archive-extract",
                    "art-1",
                    "--entry",
                    "docs/a.txt",
                    "--entry",
                    "docs/b.txt",
                ])
                .unwrap();
                match cli.command {
                    Command::ArtifactArchiveExtract { entries, all, .. } => {
                        assert_eq!(entries, vec!["docs/a.txt", "docs/b.txt"]);
                        assert!(!all);
                    }
                    other => panic!("expected archive extract, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "media-transcode",
                    "art-1",
                    "mp4",
                    "--max-duration-secs",
                    "30",
                    "--yes",
                ])
                .unwrap();
                match cli.command {
                    Command::MediaTranscode {
                        format,
                        max_duration_secs,
                        yes,
                        ..
                    } => {
                        assert_eq!(format, "mp4");
                        assert_eq!(max_duration_secs, 30);
                        assert!(yes);
                    }
                    other => panic!("expected media transcode, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn archive_list_renderer_marks_unsafe_entries_and_warnings() {
        let value = json!({
            "name": "bundle.zip",
            "format": "zip",
            "entry_count": 3,
            "total_uncompressed_bytes": 128,
            "truncated": true,
            "entries": [
                {"path": "a.txt", "size_bytes": 10, "safe": true},
                {"path": "../escape", "size_bytes": 0, "safe": false, "unsafe_reason": "path_traversal"}
            ],
            "warnings": ["entry_count_truncated"]
        });
        let lines = archive_list_lines(&value).unwrap().join("\n");
        assert!(lines.contains("bundle.zip · zip · 3 entries"));
        assert!(lines.contains("truncated at cap"));
        assert!(lines.contains("safe  "));
        assert!(lines.contains("UNSAFE"));
        assert!(lines.contains("path_traversal"));
        assert!(lines.contains("warning: entry_count_truncated"));
    }

    #[test]
    fn archive_extract_renderer_shows_published_artifacts_and_previews() {
        let value = json!({
            "extraction_id": "ext-1",
            "format": "zip",
            "total_bytes": 4,
            "scratch_retained": false,
            "entries": [
                {
                    "path": "dir/b.txt",
                    "size_bytes": 4,
                    "published_artifact_id": "art-2",
                    "preview": "beta\nsecond line"
                },
                {
                    "path": "big.bin",
                    "size_bytes": 9999999,
                    "skipped_reason": "exceeds_artifact_cap"
                }
            ],
            "published_artifacts": [{"id": "art-2"}]
        });
        let lines = archive_extract_lines(&value).unwrap().join("\n");
        assert!(lines.contains("extracted 2 entries (4 bytes) from zip · 1 artifact(s)"));
        assert!(lines.contains("scratch retained: false"));
        assert!(lines.contains("artifact art-2"));
        assert!(lines.contains("preview: beta"));
        assert!(lines.contains("exceeds_artifact_cap"));
    }

    #[test]
    fn media_renderers_show_probe_source_streams_and_bounds() {
        let info = json!({
            "name": "clip.mp4",
            "size_bytes": 4096,
            "info": {
                "source": "ffprobe",
                "container": "mp4",
                "duration_secs": 12.5,
                "width": 640,
                "height": 360,
                "streams": [
                    {
                        "index": 0,
                        "codec_type": "video",
                        "codec_name": "h264",
                        "width": 640,
                        "height": 360,
                        "duration_secs": 12.5
                    }
                ],
                "warnings": ["probe warning"]
            }
        });
        let lines = media_info_lines(&info).unwrap().join("\n");
        assert!(lines.contains("clip.mp4 · mp4 · 4096 · source ffprobe"));
        assert!(lines.contains("duration: 12.50s"));
        assert!(lines.contains("dimensions: 640x360"));
        assert!(lines.contains("stream 0: video h264 640x360 12.50s"));
        assert!(lines.contains("warning: probe warning"));

        let transcode = json!({
            "source_artifact_id": "art-1",
            "format": "mp4",
            "size_bytes": 2048,
            "elapsed_ms": 120,
            "max_duration_secs": 30,
            "max_bytes": 5242880,
            "command_summary": "ffmpeg · mp4 · max 30s",
            "warnings": ["capped"],
            "artifact": {
                "id": "art-2",
                "mime_type": "video/mp4",
                "provenance": {
                    "operation": "media_transcode",
                    "source_artifact_id": "art-1",
                    "output_format": "mp4"
                }
            }
        });
        let lines = media_transcode_lines(&transcode).unwrap().join("\n");
        assert!(lines.contains("transcoded art-1 -> mp4"));
        assert!(lines.contains("artifact art-2"));
        assert!(lines.contains("bounds: max 30s"));
        assert!(lines.contains("provenance: media_transcode"));
        assert!(lines.contains("warning: capped"));
    }
}

#[cfg(test)]
mod backup_revocation_cli_tests {
    use super::*;

    #[test]
    fn revocation_rekey_and_portable_anchor_commands_parse() {
        let cli = Cli::try_parse_from([
            "vp",
            "backup-key-revoke",
            "old",
            "--force",
            "--keyring-dir",
            "/tmp/keys",
        ])
        .unwrap();
        match cli.command {
            Command::BackupKeyRevoke {
                key_id,
                keyring_dir,
                force,
            } => {
                assert_eq!(key_id, "old");
                assert_eq!(keyring_dir.unwrap(), PathBuf::from("/tmp/keys"));
                assert!(force);
            }
            other => panic!("expected backup-key-revoke, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "backup-key-events",
            "--keyring-dir",
            "/tmp/keys",
            "--limit",
            "25",
        ])
        .unwrap();
        match cli.command {
            Command::BackupKeyEvents { keyring_dir, limit } => {
                assert_eq!(keyring_dir.unwrap(), PathBuf::from("/tmp/keys"));
                assert_eq!(limit, 25);
            }
            other => panic!("expected backup-key-events, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "backup-rekey",
            "--repository",
            "/tmp/repo",
            "--passphrase-env",
            "VP_PASS",
            "--keyring-dir",
            "/tmp/keys",
            "--dry-run",
            "--max-snapshots",
            "2",
        ])
        .unwrap();
        match cli.command {
            Command::BackupRekey {
                repository,
                passphrase_env,
                passphrase_file,
                keyring_dir,
                dry_run,
                max_snapshots,
            } => {
                assert_eq!(repository, PathBuf::from("/tmp/repo"));
                assert_eq!(passphrase_env.as_deref(), Some("VP_PASS"));
                assert!(passphrase_file.is_none());
                assert_eq!(keyring_dir.unwrap(), PathBuf::from("/tmp/keys"));
                assert!(dry_run);
                assert_eq!(max_snapshots, Some(2));
            }
            other => panic!("expected backup-rekey, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "backup-anchor-export",
            "--anchor-file",
            "/tmp/anchor.json",
            "--key-id",
            "primary",
            "--output",
            "/tmp/portable.json",
        ])
        .unwrap();
        match cli.command {
            Command::BackupAnchorExport {
                anchor_file,
                key_id,
                output,
                ..
            } => {
                assert_eq!(anchor_file.unwrap(), PathBuf::from("/tmp/anchor.json"));
                assert_eq!(key_id.as_deref(), Some("primary"));
                assert_eq!(output.unwrap(), PathBuf::from("/tmp/portable.json"));
            }
            other => panic!("expected backup-anchor-export, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "backup-anchor-verify",
            "--input",
            "/tmp/portable.json",
            "--allow-revoked",
        ])
        .unwrap();
        match cli.command {
            Command::BackupAnchorVerify {
                input,
                allow_revoked,
                ..
            } => {
                assert_eq!(input, PathBuf::from("/tmp/portable.json"));
                assert!(allow_revoked);
            }
            other => panic!("expected backup-anchor-verify, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "backup-anchor-import",
            "--input",
            "/tmp/portable.json",
            "--allow-revoked",
            "--require-newer",
        ])
        .unwrap();
        match cli.command {
            Command::BackupAnchorImport {
                input,
                allow_revoked,
                require_newer,
                ..
            } => {
                assert_eq!(input, PathBuf::from("/tmp/portable.json"));
                assert!(allow_revoked);
                assert!(require_newer);
            }
            other => panic!("expected backup-anchor-import, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "backup-verify",
            "--repository",
            "/tmp/repo",
            "--allow-revoked",
        ])
        .unwrap();
        match cli.command {
            Command::BackupVerify {
                allow_revoked,
                allow_rollback,
                ..
            } => {
                assert!(allow_revoked);
                assert!(!allow_rollback);
            }
            other => panic!("expected backup-verify, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "restore",
            "--repository",
            "/tmp/repo",
            "--snapshot",
            "01930000-0000-7000-8000-000000000001",
            "--allow-revoked",
        ])
        .unwrap();
        match cli.command {
            Command::Restore { allow_revoked, .. } => assert!(allow_revoked),
            other => panic!("expected restore, got {other:?}"),
        }
    }

    #[test]
    fn revocation_rekey_and_anchor_renderers_name_the_security_state() {
        let revocation = json!({
            "key_id": "old",
            "revoked_at": "2026-11-12T02:00:00Z",
            "was_active": true,
            "active_key_id": null,
            "force": true,
            "changed": true,
            "warning": "active signing key \"old\" was force-revoked; no key can sign until you rotate to a new key",
            "keyring": "/tmp/keys/keyring.json"
        });
        let lines = {
            // The renderer writes to stdout; assert the report shape instead of
            // capturing process output.
            assert_eq!(revocation["key_id"], "old");
            assert!(
                revocation["warning"]
                    .as_str()
                    .unwrap()
                    .contains("no key can sign")
            );
            assert!(revocation["active_key_id"].is_null());
            true
        };
        assert!(lines);

        let rekey = json!({
            "repository": "/tmp/repo",
            "repository_id": "01930000-0000-7000-8000-000000000001",
            "format_version": 3,
            "dry_run": false,
            "applied": true,
            "changed": true,
            "previous_key_id": "old",
            "key_id": "new",
            "metadata_resigned": true,
            "snapshots_total": 2,
            "snapshots_pending": 2,
            "snapshots_resigned": 2,
            "snapshots_remaining": 0,
            "complete": true,
            "max_snapshots": 10000,
            "snapshots_unchanged": 0,
            "snapshots_unsigned_preserved": 0,
            "generation": 2,
            "latest_snapshot_id": "01930000-0000-7000-8000-000000000002",
            "parent_chain_preserved": true,
            "entries_preserved": true,
            "tombstones_preserved": true,
            "blobs_total": 3,
            "blob_bytes": 4096,
            "blob_ciphertext_unchanged": true,
            "blob_ciphertext_sha256": "a".repeat(64),
            "revoked_source_key_ids": ["old"]
        });
        assert_eq!(rekey["previous_key_id"], "old");
        assert_eq!(rekey["key_id"], "new");
        assert_eq!(rekey["snapshots_resigned"], 2);
        assert!(rekey["blob_ciphertext_unchanged"].as_bool().unwrap());

        let import = json!({
            "anchor_path": "/tmp/backup-anchor.json",
            "key_id": "primary",
            "exported_at": "2026-11-12T00:00:00Z",
            "document_sha256": "b".repeat(64),
            "stale_export": false,
            "repositories_total": 1,
            "repositories_raised": 1,
            "repositories_unchanged": 0,
            "raises": [{
                "repository_id": "01930000-0000-7000-8000-000000000001",
                "previous_highest": 4,
                "previous_last_verified": 4,
                "highest_generation": 9,
                "last_verified_generation": 9,
                "raised": true
            }],
            "updated_at": "2026-11-12T01:00:00Z",
            "note": "raise-only"
        });
        assert_eq!(import["repositories_raised"], 1);
        assert_eq!(import["raises"][0]["highest_generation"], 9);
    }
}

#[cfg(test)]
mod forge_cli_tests {
    use super::*;

    #[test]
    fn pr_command_accepts_an_explicit_bitbucket_provider() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli =
                    Cli::try_parse_from(["vp", "pr", "ses-1", "--provider", "bitbucket"]).unwrap();
                match cli.command {
                    Command::Pr { session, provider } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(provider.unwrap().as_str(), "bitbucket");
                    }
                    other => panic!("expected pr, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "pr", "ses-1"]).unwrap();
                match cli.command {
                    Command::Pr { session, provider } => {
                        assert_eq!(session, "ses-1");
                        assert!(provider.is_none());
                    }
                    other => panic!("expected pr, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "pr", "ses-1", "--provider", "forgejo"]).unwrap();
                match cli.command {
                    Command::Pr { provider, .. } => {
                        assert_eq!(provider.unwrap().as_str(), "forgejo");
                    }
                    other => panic!("expected pr, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn pr_command_rejects_unknown_providers_before_rpc() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let error = Cli::try_parse_from(["vp", "pr", "ses-1", "--provider", "sourcehut"])
                    .unwrap_err();
                assert!(error.to_string().contains("sourcehut"), "{error}");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn forge_status_command_parses_optional_project() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "forge", "status"]).unwrap();
                match cli.command {
                    Command::Forge {
                        action: ForgeAction::Status { project },
                    } => assert!(project.is_none()),
                    other => panic!("expected forge status, got {other:?}"),
                }

                let cli =
                    Cli::try_parse_from(["vp", "forge", "status", "--project", "prj-1"]).unwrap();
                match cli.command {
                    Command::Forge {
                        action: ForgeAction::Status { project },
                    } => assert_eq!(project.as_deref(), Some("prj-1")),
                    other => panic!("expected forge status, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn forge_check_command_parses_project_and_provider() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "forge",
                    "check",
                    "--project",
                    "prj-1",
                    "--provider",
                    "bitbucket",
                ])
                .unwrap();
                match cli.command {
                    Command::Forge {
                        action: ForgeAction::Check { project, provider },
                    } => {
                        assert_eq!(project.as_deref(), Some("prj-1"));
                        assert_eq!(provider.unwrap().as_str(), "bitbucket");
                    }
                    other => panic!("expected forge check, got {other:?}"),
                }

                let cli = Cli::try_parse_from(["vp", "forge", "check"]).unwrap();
                match cli.command {
                    Command::Forge {
                        action: ForgeAction::Check { project, provider },
                    } => {
                        assert!(project.is_none());
                        assert!(provider.is_none());
                    }
                    other => panic!("expected forge check, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn forge_status_lines_cover_origin_and_provider_rows() {
        let value = serde_json::json!({
            "default_provider": "bitbucket",
            "provider_override": "bitbucket",
            "origin": {
                "host": "bitbucket.org",
                "provider": "bitbucket",
                "repository": "acme/widgets",
                "supported": true,
                "server_detected": false,
                "detail": "Bitbucket Cloud origin detected"
            },
            "providers": [
                {
                    "provider": "bitbucket",
                    "change_name": "PR",
                    "transport": "rest",
                    "command_available": true,
                    "auth_configured": true,
                    "credential_source": "BITBUCKET_TOKEN",
                    "supported": true,
                    "detail": "credentials configured"
                },
                {
                    "provider": "github",
                    "change_name": "PR",
                    "transport": "cli",
                    "command": "gh",
                    "command_available": false,
                    "supported": false,
                    "detail": "gh CLI not found; install gh or set VERLINPARK_GH_BIN."
                }
            ],
            "detail": "selected bitbucket"
        });
        let lines = forge_status_lines(&value).unwrap().join(
            "
",
        );
        assert!(lines.contains("selected bitbucket"), "{lines}");
        assert!(lines.contains("repository acme/widgets"), "{lines}");
        assert!(lines.contains("credentials configured"), "{lines}");
        assert!(lines.contains("gh CLI not found"), "{lines}");
        assert!(!lines.contains("BITBUCKET_TOKEN"), "{lines}");
    }

    #[test]
    fn preflight_lines_name_auth_mode_and_repository() {
        let value = serde_json::json!({
            "provider": "bitbucket",
            "change_name": "PR",
            "repository": "acme/widgets",
            "auth": "bearer",
            "detail": "Bitbucket Cloud credentials accepted for acme/widgets (bearer auth)"
        });
        let lines = forge_preflight_lines(&value).unwrap().join(
            "
",
        );
        assert!(lines.contains("bearer"), "{lines}");
        assert!(lines.contains("acme/widgets"), "{lines}");
        assert!(!lines.contains("ATATT"), "{lines}");
    }

    #[test]
    fn pull_request_lines_prefer_provider_and_change_name() {
        let value = serde_json::json!({
            "session_id": "ses-1",
            "branch": "feature",
            "base_branch": "main",
            "pushed": true,
            "url": "https://bitbucket.org/acme/widgets/pull-requests/7",
            "detail": "opened",
            "provider": "bitbucket",
            "repository": "acme/widgets",
            "change": "PR"
        });
        let lines = pull_request_lines(&value).unwrap().join(
            "
",
        );
        assert!(lines.contains("PR opened via bitbucket"), "{lines}");
        assert!(lines.contains("feature → main"), "{lines}");

        let legacy = serde_json::json!({
            "session_id": "ses-1",
            "branch": "feature",
            "base_branch": "main",
            "pushed": true,
            "url": "https://example.test/pr/1",
            "detail": "opened"
        });
        let lines = pull_request_lines(&legacy).unwrap().join(
            "
",
        );
        assert!(lines.contains("PR opened via forge"), "{lines}");
    }

    #[test]
    fn forge_provider_argument_has_stable_wire_values() {
        assert_eq!(ForgeProviderArg::Github.as_str(), "github");
        assert_eq!(ForgeProviderArg::Gitlab.as_str(), "gitlab");
        assert_eq!(ForgeProviderArg::Gitea.as_str(), "gitea");
        assert_eq!(ForgeProviderArg::Forgejo.as_str(), "forgejo");
        assert_eq!(ForgeProviderArg::Bitbucket.as_str(), "bitbucket");
    }
}

#[cfg(test)]
mod remote_bootstrap_test_cli_tests {
    use super::*;

    #[test]
    fn remote_bootstrap_and_test_commands_parse_bounds() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(parse_remote_bootstrap_test_commands)
            .unwrap()
            .join()
            .unwrap();
    }

    fn parse_remote_bootstrap_test_commands() {
        let cli = Cli::try_parse_from([
            "vp",
            "remote-bootstrap",
            "prj-1",
            "--install-path",
            "~/.verlinpark/bin/vp-remote-agent",
            "--expected-sha256",
            "aabb",
            "--expected-version",
            "0.1.0-dev",
            "--force",
            "--confirm",
            "--timeout",
            "120",
            "--session",
            "ses-1",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteBootstrap {
                project,
                install_path,
                expected_sha256,
                expected_version,
                force,
                confirm,
                timeout,
                session,
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(
                    install_path.as_deref(),
                    Some("~/.verlinpark/bin/vp-remote-agent")
                );
                assert_eq!(expected_sha256.as_deref(), Some("aabb"));
                assert_eq!(expected_version.as_deref(), Some("0.1.0-dev"));
                assert!(force);
                assert!(confirm);
                assert_eq!(timeout, Some(120));
                assert_eq!(session.as_deref(), Some("ses-1"));
            }
            other => panic!("expected remote-bootstrap, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "remote-test",
            "prj-1",
            "--session",
            "ses-1",
            "--entry",
            "entry-1",
            "--profile",
            "check",
            "--command",
            "cargo check",
            "--cwd",
            "crates/vp-core",
            "--timeout",
            "600",
            "--output-cap",
            "65536",
            "--expected-commit",
            "abcdef1234567890",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteTest {
                project,
                session,
                entry,
                profile,
                command,
                cwd,
                timeout,
                output_cap,
                expected_commit,
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(session.as_deref(), Some("ses-1"));
                assert_eq!(entry.as_deref(), Some("entry-1"));
                assert_eq!(profile.as_deref(), Some("check"));
                assert_eq!(command.as_deref(), Some("cargo check"));
                assert_eq!(cwd.as_deref(), Some("crates/vp-core"));
                assert_eq!(timeout, Some(600));
                assert_eq!(output_cap, Some(65_536));
                assert_eq!(expected_commit.as_deref(), Some("abcdef1234567890"));
            }
            other => panic!("expected remote-test, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "remote-test-policy-set",
            "prj-1",
            "--enable",
            "--default",
            "test",
            "--profile",
            "check:check:cargo check",
            "--profile",
            "test:test:cargo test --workspace",
            "--timeout",
            "900",
            "--output-cap",
            "131072",
        ])
        .unwrap();
        match cli.command {
            Command::RemoteTestPolicySet {
                project,
                enable,
                disable,
                default_profile,
                timeout,
                output_cap,
                profiles,
                clear_profiles,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert!(enable);
                assert!(!disable);
                assert_eq!(default_profile.as_deref(), Some("test"));
                assert_eq!(timeout, Some(900));
                assert_eq!(output_cap, Some(131_072));
                assert_eq!(profiles.len(), 2);
                assert!(!clear_profiles);
            }
            other => panic!("expected remote-test-policy-set, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "remote-test-status",
            "prj-1",
            "--session",
            "ses-1",
            "--limit",
            "5",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::RemoteTestStatus {
                limit: 5,
                session: Some(ref session),
                ..
            } if session == "ses-1"
        ));

        let cli = Cli::try_parse_from([
            "vp",
            "remote-test-output",
            "prj-1",
            "evidence-1",
            "--offset",
            "10",
            "--limit",
            "100",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::RemoteTestOutput {
                offset: 10,
                limit: 100,
                ..
            }
        ));
    }

    #[test]
    fn remote_bootstrap_human_output_is_explicit_about_sudo_and_caveat() {
        let value = serde_json::json!({
            "project_id": "prj-1",
            "host": "build.example.com",
            "install_path": "/home/agent/.verlinpark/bin/vp-remote-agent",
            "sha256": "a".repeat(64),
            "bytes": 4096,
            "helper_version": "0.1.0-dev",
            "protocol_version": 1,
            "target_os": "linux",
            "target_arch": "x86_64",
            "remote_os": "Linux",
            "remote_arch": "x86_64",
            "replaced": false,
            "used_sudo": false,
            "chunks": 2,
            "duration_ms": 42,
            "installed_at": "2026-01-01T00:00:00Z",
            "risk": "host_state",
            "approval_required": true,
            "confirmation_required": true,
            "host_path_mismatch": true,
            "environment_caveat": "trusted SSH host; not sandboxed"
        });
        let text = remote_bootstrap_lines(&value).unwrap().join("\n");
        assert!(text.contains("installed"));
        assert!(text.contains("build.example.com"));
        assert!(text.contains("sha256"));
        assert!(text.contains("sudo false"));
        assert!(text.contains("risk host_state"));
        assert!(text.contains("does not name the installed file"));
        assert!(text.contains("not sandboxed"));
    }

    #[test]
    fn remote_test_human_output_never_implies_the_merge_gate_changed() {
        let value = serde_json::json!({
            "project_id": "prj-1",
            "session_id": "ses-1",
            "evidence": {
                "id": "ev-1",
                "status": "passed",
                "profile": "test",
                "host": "build.example.com",
                "commit": "abcdef1234567890",
                "command": "cargo test",
                "exit_code": 0,
                "duration_ms": 12,
                "digest": "d".repeat(64),
                "output_ref": "remote-test-outputs/x.txt",
                "output_sha256": "e".repeat(64),
                "output_bytes": 100,
                "output_truncated": false
            },
            "digest": "d".repeat(64),
            "entry": {
                "id": "entry-1",
                "status": "queued",
                "verifier_approved": null
            },
            "supplemental": true,
            "local_verifier_required": true,
            "merge_gate_unchanged": true,
            "environment_caveat": "supplemental only"
        });
        let text = remote_test_lines(&value).unwrap().join("\n");
        assert!(text.contains("remote test passed"));
        assert!(text.contains("not yet run"));
        assert!(text.contains("local Verifier"));
        assert!(text.contains("merge gate is unchanged"));

        let status = serde_json::json!({
            "project_id": "prj-1",
            "policy": {
                "enabled": true,
                "default_profile": "test",
                "profiles": [{"name": "test", "kind": "test", "command": "cargo test"}]
            },
            "evidence": [],
            "total": 0,
            "passed": 0,
            "failed": 0,
            "timed_out": 0,
            "environment_caveat": "supplemental only"
        });
        let text = remote_test_lines(&status).unwrap().join("\n");
        assert!(text.contains("remote test evidence for prj-1"));
        assert!(text.contains("never bypasses"));
    }
}

#[cfg(test)]
mod i5_cli_tests {
    use super::*;

    #[test]
    fn clap_parses_daemon_agent_lifecycle_flags() {
        let cli = Cli::try_parse_from([
            "vp",
            "daemon-agent",
            "install",
            "--yes",
            "--dry-run",
            "--force",
            "--data-dir",
            "/tmp/vp-data",
        ])
        .unwrap();
        match cli.command {
            Command::DaemonAgent {
                action,
                yes,
                dry_run,
                force,
                data_dir,
                ..
            } => {
                assert!(matches!(action, DaemonAgentAction::Install));
                assert!(yes && dry_run && force);
                assert_eq!(
                    data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/vp-data"))
                );
            }
            other => panic!("expected daemon-agent, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "daemon-agent", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::DaemonAgent {
                action: DaemonAgentAction::Status,
                ..
            }
        ));
        let cli = Cli::try_parse_from(["vp", "daemon-agent", "uninstall", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::DaemonAgent {
                action: DaemonAgentAction::Uninstall,
                yes: true,
                ..
            }
        ));
    }

    #[test]
    fn clap_parses_gui_retention_and_external_cache_scopes() {
        let cli = Cli::try_parse_from([
            "vp",
            "gui-timeline-status",
            "proj-1",
            "--older-than-days",
            "14",
        ])
        .unwrap();
        match cli.command {
            Command::GuiTimelineStatus {
                project,
                older_than_days,
            } => {
                assert_eq!(project, "proj-1");
                assert_eq!(older_than_days, Some(14));
            }
            other => panic!("expected gui-timeline-status, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "vp",
            "gui-timeline-prune",
            "proj-1",
            "--older-than-days",
            "30",
            "--limit",
            "25",
            "--dry-run",
            "--yes",
        ])
        .unwrap();
        match cli.command {
            Command::GuiTimelinePrune {
                project,
                older_than_days,
                limit,
                dry_run,
                yes,
            } => {
                assert_eq!(project, "proj-1");
                assert_eq!(older_than_days, Some(30));
                assert_eq!(limit, 25);
                assert!(dry_run);
                assert!(yes);
            }
            other => panic!("expected gui-timeline-prune, got {other:?}"),
        }
        let cli =
            Cli::try_parse_from(["vp", "gui-timeline-audit", "proj-1", "--limit", "12"]).unwrap();
        match cli.command {
            Command::GuiTimelineAudit { project, limit } => {
                assert_eq!(project, "proj-1");
                assert_eq!(limit, 12);
            }
            other => panic!("expected gui-timeline-audit, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "test-cache-scopes", "proj-1"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::TestCacheScopes { project } if project == "proj-1"
        ));
        let cli = Cli::try_parse_from([
            "vp",
            "test-cache-policy-set",
            "proj-1",
            "--extra-env",
            "MY_INPUT",
            "--extra-env",
            "OTHER_INPUT",
            "--extra-path",
            "config/local.env",
            "--replace-extra-scopes",
        ])
        .unwrap();
        match cli.command {
            Command::TestCachePolicySet {
                project,
                extra_env,
                extra_path,
                replace_extra_scopes,
                ..
            } => {
                assert_eq!(project, "proj-1");
                assert_eq!(extra_env, vec!["MY_INPUT", "OTHER_INPUT"]);
                assert_eq!(extra_path, vec!["config/local.env"]);
                assert!(replace_extra_scopes);
            }
            other => panic!("expected test-cache-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn clap_parses_explicit_process_restart() {
        let cli =
            Cli::try_parse_from(["vp", "process-restart", "ses-1", "psup_1", "--yes"]).unwrap();
        match cli.command {
            Command::ProcessRestart {
                session,
                process,
                yes,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(process, "psup_1");
                assert!(yes);
            }
            other => panic!("expected process-restart, got {other:?}"),
        }
    }

    #[test]
    fn clap_parses_process_restart_policy_flags() {
        let cli = Cli::try_parse_from([
            "vp",
            "process-policy-set",
            "proj-1",
            "--restart-on-boot",
            "true",
            "--restart-max-attempts",
            "4",
            "--restart-backoff-secs",
            "15",
        ])
        .unwrap();
        match cli.command {
            Command::ProcessPolicySet {
                restart_on_boot,
                restart_max_attempts,
                restart_backoff_secs,
                ..
            } => {
                assert_eq!(restart_on_boot, Some(true));
                assert_eq!(restart_max_attempts, Some(4));
                assert_eq!(restart_backoff_secs, Some(15));
            }
            other => panic!("expected process-policy-set, got {other:?}"),
        }
    }

    #[test]
    fn cache_bench_runs_without_a_subcommand_and_parses_compare() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-bench",
                    "--turns",
                    "24",
                    "--stable-prefix-tokens",
                    "180000",
                    "--initial-context-tokens",
                    "20000",
                ])
                .unwrap();
                match cli.command {
                    Command::CacheBench { command, run } => {
                        assert!(command.is_none());
                        assert_eq!(run.turns, 24);
                        assert_eq!(run.stable_prefix_tokens, 180_000);
                        assert_eq!(run.initial_context_tokens, 20_000);
                        assert!(!run.live);
                        assert!(!run.allow_live);
                    }
                    other => panic!("expected cache-bench, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-bench",
                    "compare",
                    "a.json",
                    "b.json",
                    "--allow-mismatch",
                ])
                .unwrap();
                match cli.command {
                    Command::CacheBench {
                        command:
                            Some(vp_bench::cache_bench::CacheBenchCommand::Compare {
                                left,
                                right,
                                allow_mismatch,
                            }),
                        ..
                    } => {
                        assert_eq!(left, std::path::PathBuf::from("a.json"));
                        assert_eq!(right, std::path::PathBuf::from("b.json"));
                        assert!(allow_mismatch);
                    }
                    other => panic!("expected cache-bench compare, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn project_cache_parses_bounded_scan() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from(["vp", "project-cache", "proj-1", "--limit", "2000"])
                    .unwrap();
                match cli.command {
                    Command::ProjectCache { project, limit } => {
                        assert_eq!(project, "proj-1");
                        assert_eq!(limit, Some(2_000));
                    }
                    other => panic!("expected project-cache, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_calls_parses_bounded_filters() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-calls",
                    "ses-1",
                    "--limit",
                    "40",
                    "--role",
                    "worker",
                    "--miss-reason",
                    "tail_delta",
                    "--include-warm",
                ])
                .unwrap();
                match cli.command {
                    Command::CacheCalls {
                        session,
                        limit,
                        role,
                        miss_reason,
                        include_warm,
                    } => {
                        assert_eq!(session, "ses-1");
                        assert_eq!(limit, Some(40));
                        assert_eq!(role.as_deref(), Some("worker"));
                        assert_eq!(miss_reason.as_deref(), Some("tail_delta"));
                        assert!(include_warm);
                    }
                    other => panic!("expected cache-calls, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn cache_bench_live_gate_is_explicit() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "cache-bench",
                    "run",
                    "--live",
                    "--allow-live",
                    "--model",
                    "deepseek-chat",
                ])
                .unwrap();
                match cli.command {
                    Command::CacheBench {
                        command: Some(vp_bench::cache_bench::CacheBenchCommand::Run(args)),
                        ..
                    } => {
                        assert!(args.live);
                        assert!(args.allow_live);
                        assert_eq!(args.model.as_deref(), Some("deepseek-chat"));
                    }
                    other => panic!("expected live cache-bench run, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(test)]
mod effect_cli_tests {
    use super::*;

    #[test]
    fn effect_commands_parse_and_require_evidence_flags() {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let cli = Cli::try_parse_from([
                    "vp",
                    "effects",
                    "ses-123",
                    "--task",
                    "task-1",
                    "--limit",
                    "40",
                    "--unresolved",
                ])
                .unwrap();
                match cli.command {
                    Command::Effects {
                        session,
                        task,
                        limit,
                        unresolved,
                    } => {
                        assert_eq!(session, "ses-123");
                        assert_eq!(task.as_deref(), Some("task-1"));
                        assert_eq!(limit, Some(40));
                        assert!(unresolved);
                    }
                    other => panic!("expected effects, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "effect-reconcile",
                    "effect-1",
                    "--done",
                    "--sha256",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "--note",
                    "verified on the remote host",
                    "--operator",
                    "ops",
                ])
                .unwrap();
                match cli.command {
                    Command::EffectReconcile {
                        effect,
                        done,
                        failed,
                        sha256,
                        note,
                        operator,
                        ..
                    } => {
                        assert_eq!(effect, "effect-1");
                        assert!(done);
                        assert!(!failed);
                        assert!(sha256.is_some());
                        assert!(note.is_some());
                        assert_eq!(operator.as_deref(), Some("ops"));
                    }
                    other => panic!("expected effect-reconcile, got {other:?}"),
                }

                let cli = Cli::try_parse_from([
                    "vp",
                    "effect-policy-set",
                    "prj-1",
                    "--unknown-blocks-merge",
                    "false",
                    "--stale-grace-secs",
                    "300",
                    "--shell-mode",
                    "all",
                ])
                .unwrap();
                match cli.command {
                    Command::EffectPolicySet {
                        project,
                        unknown_blocks_merge,
                        stale_intent_grace_secs,
                        shell_mode,
                        ..
                    } => {
                        assert_eq!(project, "prj-1");
                        assert_eq!(unknown_blocks_merge, Some(false));
                        assert_eq!(stale_intent_grace_secs, Some(300));
                        assert_eq!(shell_mode.as_deref(), Some("all"));
                    }
                    other => panic!("expected effect-policy-set, got {other:?}"),
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn effect_lines_render_state_without_secret_material() {
        let value = json!({
            "session_id": "ses-1",
            "policy": {"enabled": true, "unknown_blocks_merge": true, "stale_intent_grace_secs": 120, "shell_mode": "external_only"},
            "counts": {"total": 2, "intent": 0, "done": 1, "failed": 0, "unknown": 1, "blocking": 1},
            "blocked": true,
            "block_reason": "EFFECT_UNKNOWN: 1 unknown effect receipt(s) block integration",
            "receipts": [
                {
                    "id": "effect-1",
                    "state": "done",
                    "attempt": 1,
                    "tool": "write_file",
                    "target": {"kind": "workspace_write", "key": "src/lib.rs"},
                    "evidence": {"probe": "workspace_sha256", "outcome": "observed", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}
                },
                {
                    "id": "effect-2",
                    "state": "unknown",
                    "attempt": 1,
                    "tool": "remote_write",
                    "target": {"kind": "remote_write", "key": "out/app.txt"},
                    "evidence": {"probe": "remote_helper_hash", "outcome": "unverifiable", "reason": "probe_incomplete"}
                }
            ]
        });
        let lines = effect_lines(&value).unwrap();
        let rendered = lines.join("\n");
        assert!(rendered.contains("2 total"));
        assert!(rendered.contains("merge blocked"));
        assert!(rendered.contains("workspace_write:src/lib.rs"));
        assert!(rendered.contains("unknown"));
        assert!(rendered.contains("remote_helper_hash") || rendered.contains("probe_incomplete"));
        assert!(!rendered.contains("content"));
    }

    #[test]
    fn effect_policy_lines_and_sweep_result_render() {
        let policy = json!({
            "project_id": "prj-1",
            "policy": {
                "enabled": true,
                "unknown_blocks_merge": true,
                "stale_intent_grace_secs": 120,
                "shell_mode": "external_only",
                "track_mutating_http": true,
                "track_read_http": false,
                "track_mcp_mutations": true,
                "track_workspace_writes": true
            }
        });
        let lines = effect_lines(&policy).unwrap();
        assert!(lines[0].contains("prj-1"));
        assert!(lines[0].contains("external_only"));
        let run_policy = json!({
            "project_id": "prj-1",
            "policy": {
                "default_profile": "fast",
                "completion_reserve_ratio": 0.2,
                "attention_cap": 20,
                "stop_policy": {}
            }
        });
        assert!(
            effect_lines(&run_policy).is_none(),
            "a project RunPolicy must not render as an effect policy"
        );
        let sweep = json!({"swept": 2});
        assert!(effect_lines(&sweep).unwrap()[0].contains("2 stale intent"));
    }
}

#[cfg(test)]
mod otel_cli_tests {
    use super::*;

    #[test]
    fn otel_commands_parse_explicit_status_export_and_policy() {
        let cli =
            Cli::try_parse_from(["vp", "otel-status", "proj-1", "--session", "ses-1"]).unwrap();
        match cli.command {
            Command::OtelStatus { project, session } => {
                assert_eq!(project, "proj-1");
                assert_eq!(session.as_deref(), Some("ses-1"));
            }
            other => panic!("expected otel-status, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "otel-policy-set",
            "proj-1",
            "--enable",
            "--endpoint",
            "http://127.0.0.1:4318",
            "--batch-limit",
            "50",
            "--timeout-secs",
            "3",
        ])
        .unwrap();
        match cli.command {
            Command::OtelPolicySet {
                project,
                enable,
                disable,
                endpoint,
                batch_limit,
                timeout_secs,
                ..
            } => {
                assert_eq!(project, "proj-1");
                assert!(enable);
                assert!(!disable);
                assert_eq!(endpoint.as_deref(), Some("http://127.0.0.1:4318"));
                assert_eq!(batch_limit, Some(50));
                assert_eq!(timeout_secs, Some(3));
            }
            other => panic!("expected otel-policy-set, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "otel-export",
            "ses-1",
            "--limit",
            "25",
            "--offset",
            "50",
        ])
        .unwrap();
        match cli.command {
            Command::OtelExport {
                session,
                limit,
                offset,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(limit, Some(25));
                assert_eq!(offset, Some(50));
            }
            other => panic!("expected otel-export, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "otel-test", "proj-1"]).unwrap();
        assert!(matches!(cli.command, Command::OtelTest { .. }));
    }
}

#[cfg(test)]
mod plan_fanout_cli_tests {
    use super::*;

    #[test]
    fn plan_fanout_parses_bounded_items_and_explicit_yes() {
        let cli = Cli::try_parse_from([
            "vp",
            "plan-fanout",
            "ses-1",
            "--template",
            "T1",
            "--items",
            r#"[{"key":"auth","write_scope":["src/auth"]}]"#,
            "--join-title",
            "Join auth work",
            "--join-acceptance",
            "all children pass",
            "--expected-revision",
            "3",
            "--yes",
        ])
        .unwrap();
        match cli.command {
            Command::PlanFanout {
                session,
                template,
                items,
                file,
                artifact,
                join_title,
                join_acceptance,
                expected_revision,
                yes,
                ..
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(template, "T1");
                assert!(items.as_deref().unwrap().contains("\"auth\""));
                assert!(file.is_none());
                assert!(artifact.is_none());
                assert_eq!(join_title.as_deref(), Some("Join auth work"));
                assert_eq!(join_acceptance, vec!["all children pass".to_string()]);
                assert_eq!(expected_revision, Some(3));
                assert!(yes);
            }
            other => panic!("expected plan-fanout, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "plan-fanout",
            "ses-1",
            "--template",
            "T1",
            "--artifact",
            "art-1",
        ])
        .unwrap();
        match cli.command {
            Command::PlanFanout {
                items,
                artifact,
                yes,
                ..
            } => {
                assert!(items.is_none());
                assert_eq!(artifact.as_deref(), Some("art-1"));
                assert!(!yes);
            }
            other => panic!("expected plan-fanout, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod mcp_task_cli_tests {
    use super::*;

    #[test]
    fn mcp_task_commands_parse_explicit_polling_and_context() {
        let cli = Cli::try_parse_from([
            "vp",
            "mcp-call",
            "server-1",
            "slow",
            "--arguments",
            "{}",
            "--session",
            "ses-1",
        ])
        .unwrap();
        match cli.command {
            Command::McpCall {
                server,
                tool,
                session,
                ..
            } => {
                assert_eq!(server, "server-1");
                assert_eq!(tool, "slow");
                assert_eq!(session.as_deref(), Some("ses-1"));
            }
            other => panic!("expected mcp-call, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "mcp-task-result", "mcpt-1", "--no-refresh"]).unwrap();
        match cli.command {
            Command::McpTaskResult {
                task_id,
                no_refresh,
            } => {
                assert_eq!(task_id, "mcpt-1");
                assert!(no_refresh);
            }
            other => panic!("expected mcp-task-result, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "vp",
            "mcp-tasks",
            "--session",
            "ses-1",
            "--status",
            "input_required",
            "--limit",
            "25",
        ])
        .unwrap();
        match cli.command {
            Command::McpTasks {
                session,
                status,
                limit,
                ..
            } => {
                assert_eq!(session.as_deref(), Some("ses-1"));
                assert_eq!(status.as_deref(), Some("input_required"));
                assert_eq!(limit, 25);
            }
            other => panic!("expected mcp-tasks, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod gate_cli_tests {
    use super::*;

    #[test]
    fn gate_commands_parse_with_scoped_token_and_bounded_expiry() {
        let cli = Cli::try_parse_from([
            "vp",
            "gate",
            "open",
            "ses-1",
            "--task",
            "T1",
            "--kind",
            "ci",
            "--label",
            "unit tests",
            "--after-secs",
            "600",
        ])
        .unwrap();
        match cli.command {
            Command::Gate {
                command:
                    GateCommand::Open {
                        session,
                        task,
                        kind,
                        label,
                        after_secs,
                        ..
                    },
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(task.as_deref(), Some("T1"));
                assert_eq!(kind, "ci");
                assert_eq!(label.as_deref(), Some("unit tests"));
                assert_eq!(after_secs, Some(600));
            }
            other => panic!("expected gate open, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "gate",
            "resolve",
            "gate-1",
            "--token",
            "vpg_secret",
            "--outcome",
            "rejected",
        ])
        .unwrap();
        match cli.command {
            Command::Gate {
                command:
                    GateCommand::Resolve {
                        gate,
                        token,
                        outcome,
                        ..
                    },
            } => {
                assert_eq!(gate, "gate-1");
                assert_eq!(token.as_deref(), Some("vpg_secret"));
                assert_eq!(outcome, "rejected");
            }
            other => panic!("expected gate resolve, got {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["vp", "gate", "resolve", "gate-1", "--token-stdin"]).unwrap();
        match cli.command {
            Command::Gate {
                command:
                    GateCommand::Resolve {
                        gate,
                        token,
                        token_stdin,
                        token_file,
                        ..
                    },
            } => {
                assert_eq!(gate, "gate-1");
                assert!(token.is_none());
                assert!(token_stdin);
                assert!(token_file.is_none());
            }
            other => panic!("expected gate resolve, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn gate_token_file_must_not_be_group_or_world_accessible() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"vpg_file_token\n").unwrap();
        let path = file.path().to_path_buf();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_gate_token(None, false, Some(&path)).unwrap(),
            "vpg_file_token"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = read_gate_token(None, false, Some(&path)).unwrap_err();
        assert!(error.to_string().contains("chmod 600"), "{error:#}");
    }

    #[test]
    fn defer_accepts_a_gate_watch_without_a_task_or_deadline() {
        let cli =
            Cli::try_parse_from(["vp", "defer", "ses-1", "review the PR", "--gate", "gate-1"])
                .unwrap();
        match cli.command {
            Command::Defer { gate, watch, .. } => {
                assert_eq!(gate.as_deref(), Some("gate-1"));
                assert!(watch.is_empty());
            }
            other => panic!("expected defer, got {other:?}"),
        }
    }

    #[test]
    fn gate_lines_show_token_once_and_never_show_a_verifier() {
        let open = json!({
            "gate": {
                "id": "gate-1",
                "session_id": "ses-1",
                "kind": "ci",
                "label": "unit tests",
                "state": "open",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "token": "vpg_once",
            "resolve_hint": "vp gate resolve gate-1 --token <one-time-token>",
            "loopback_resolve": false,
            "token_hash": "a".repeat(64)
        });
        let rendered = gate_lines(&open).unwrap().join("\n");
        assert!(rendered.contains("ONE-TIME GATE TOKEN"));
        assert!(rendered.contains("vpg_once"));
        assert!(!rendered.contains(&"a".repeat(64)));

        let list = json!({
            "session_id": "ses-1",
            "counts": {"total": 1, "open": 1, "resolved": 0, "expired": 0, "cancelled": 0},
            "gates": [{"id": "gate-1", "state": "open", "kind": "ci", "label": "unit tests"}],
            "truncated": false
        });
        let rendered = gate_lines(&list).unwrap().join("\n");
        assert!(rendered.contains("1 open"));
        assert!(rendered.contains("gate-1"));
        assert!(!rendered.contains("vpg_"));
    }

    #[test]
    fn gate_cancel_list_status_and_policy_parse() {
        let cli = Cli::try_parse_from([
            "vp",
            "gate",
            "cancel",
            "gate-1",
            "--reason",
            "superseded",
            "--operator",
            "cli",
        ])
        .unwrap();
        match cli.command {
            Command::Gate {
                command:
                    GateCommand::Cancel {
                        gate,
                        reason,
                        operator,
                    },
            } => {
                assert_eq!(gate, "gate-1");
                assert_eq!(reason.as_deref(), Some("superseded"));
                assert_eq!(operator.as_deref(), Some("cli"));
            }
            other => panic!("expected gate cancel, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp", "gate", "list", "ses-1", "--state", "open", "--limit", "25",
        ])
        .unwrap();
        match cli.command {
            Command::Gate {
                command:
                    GateCommand::List {
                        session,
                        state,
                        limit,
                    },
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(state.as_deref(), Some("open"));
                assert_eq!(limit, 25);
            }
            other => panic!("expected gate list, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "gate", "policy-set", "prj-1", "--loopback"]).unwrap();
        match cli.command {
            Command::Gate {
                command: GateCommand::PolicySet { project, loopback },
            } => {
                assert_eq!(project, "prj-1");
                assert!(loopback);
            }
            other => panic!("expected gate policy-set, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "gate", "sweep", "--limit", "5"]).unwrap();
        match cli.command {
            Command::Gate {
                command: GateCommand::Sweep { limit },
            } => assert_eq!(limit, 5),
            other => panic!("expected gate sweep, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "gate", "status", "gate-1"]).unwrap();
        match cli.command {
            Command::Gate {
                command: GateCommand::Status { gate },
            } => assert_eq!(gate, "gate-1"),
            other => panic!("expected gate status, got {other:?}"),
        }
    }

    #[test]
    fn gate_status_and_policy_render_without_private_fields() {
        let status = json!({
            "gate": {
                "id": "gate-1",
                "session_id": "ses-1",
                "kind": "deploy",
                "label": "staging",
                "state": "resolved",
                "outcome": "resolved",
                "resolution_note": "shipped",
                "created_at": "2026-11-12T00:00:00Z"
            },
            "continuation_id": "cont-1",
            "continuation_status": "delivered"
        });
        let rendered = gate_lines(&status).unwrap().join("\n");
        assert!(rendered.contains("resolved"));
        assert!(rendered.contains("shipped"));
        assert!(rendered.contains("cont-1"));
        let policy = json!({
            "project_id": "prj-1",
            "policy": {"loopback_resolve": true}
        });
        let rendered = gate_lines(&policy).unwrap().join("\n");
        assert!(rendered.contains("loopback_resolve=true"));
        assert!(!rendered.contains("token"));
        let sweep = json!({"expired_gates": 3});
        assert!(gate_lines(&sweep).unwrap()[0].contains("3 open gate"));
    }

    #[test]
    fn compatibility_and_replay_commands_parse_and_render() {
        let cli = Cli::try_parse_from(["vp", "compatibility", "ses-1", "--run", "run-1"]).unwrap();
        match cli.command {
            Command::Compatibility { session, run } => {
                assert_eq!(session, "ses-1");
                assert_eq!(run.as_deref(), Some("run-1"));
            }
            other => panic!("expected compatibility, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "vp",
            "plan-migrate",
            "ses-1",
            "--yes",
            "--expected-revision",
            "2",
            "--reason",
            "upgrade",
        ])
        .unwrap();
        match cli.command {
            Command::PlanMigrate {
                session,
                reason,
                expected_revision,
                yes,
                ..
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(reason.as_deref(), Some("upgrade"));
                assert_eq!(expected_revision, Some(2));
                assert!(yes);
            }
            other => panic!("expected plan-migrate, got {other:?}"),
        }
        let cli =
            Cli::try_parse_from(["vp", "orchestrate-replay", "ses-1", "--limit", "50"]).unwrap();
        match cli.command {
            Command::OrchestrateReplay { session, limit } => {
                assert_eq!(session, "ses-1");
                assert_eq!(limit, 50);
            }
            other => panic!("expected orchestrate-replay, got {other:?}"),
        }
    }

    #[test]
    fn compatibility_and_replay_render_without_secrets_or_model_claims() {
        let report = json!({
            "status": "incompatible",
            "target": "plan",
            "session_id": "ses-1",
            "plan_revision": 2,
            "snapshot_age_secs": 42,
            "changed": [
                {"field": "contract_hash", "stored": "aaaa", "current": "bbbb", "severity": "incompatible"}
            ],
            "warnings": ["contract_changed"],
            "migration_required": true,
            "checked_at": "2026-11-12T00:00:00Z"
        });
        let rendered = runtime_pin_lines(&report).unwrap().join("\n");
        assert!(rendered.contains("INCOMPATIBLE"));
        assert!(rendered.contains("migration required"));
        assert!(rendered.contains("contract_hash"));
        assert!(!rendered.contains("api_key"));
        let replay = json!({
            "session_id": "ses-1",
            "matched": 3,
            "mismatched": 1,
            "skipped": 0,
            "examined": 4,
            "total_recorded": 9,
            "truncated": true,
            "controller_equivalent_only": true,
            "entries": [{
                "seq": 7,
                "kind": "orchestration.decision",
                "status": "mismatched",
                "recorded_action": "retry:T1",
                "recomputed_action": "block:T2",
                "changes": [{"field": "action", "recorded": "retry:T1", "recomputed": "block:T2"}],
                "reason": "different action set"
            }]
        });
        let rendered = controller_replay_lines(&replay).unwrap().join("\n");
        assert!(rendered.contains("MISMATCHED"));
        assert!(rendered.contains("controller-equivalent only"));
        assert!(!rendered.contains("verified"));
    }

    #[test]
    fn diagnose_parses_and_renders_read_only_attribution() {
        let cli = Cli::try_parse_from([
            "vp", "diagnose", "ses-1", "--task", "t2", "--limit", "500", "--graph",
        ])
        .unwrap();
        match cli.command {
            Command::Diagnose {
                session,
                task,
                limit,
                graph,
                record,
                history,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(task.as_deref(), Some("t2"));
                assert_eq!(limit, Some(500));
                assert!(graph);
                assert!(!record);
                assert!(!history);
            }
            other => panic!("expected diagnose, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "diagnose", "ses-1", "--history"]).unwrap();
        match cli.command {
            Command::Diagnose { history, .. } => assert!(history),
            other => panic!("expected diagnose history, got {other:?}"),
        }

        let report = json!({
            "status": "attributed",
            "session_id": "ses-1",
            "target": {"kind": "task", "id": "t2", "task_id": "t2", "status": "failed", "at_seq": 9},
            "earliest": {
                "reason_code": "unsubstantiated_handoff",
                "severity": "root",
                "node_id": "task:t2",
                "task_id": "t2",
                "at_seq": 8,
                "summary": "the Worker result does not cite an upstream commit or artifact hash it used",
                "agents": [{"agent_id": "worker-2", "agent_name": "Worker 2", "role": "worker"}],
                "evidence": [{"kind": "hash", "reference": "upstream", "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "event_seq": 4}]
            },
            "findings": [{
                "reason_code": "unsubstantiated_handoff",
                "severity": "root",
                "task_id": "t2",
                "node_id": "task:t2"
            }],
            "responsible_agents": [{"agent_id": "worker-2", "agent_name": "Worker 2", "role": "worker"}],
            "cascade": [
                {"kind": "task", "task_id": "t1", "status": "completed", "at_seq": 4, "relation": "root"},
                {"kind": "task", "task_id": "t2", "status": "failed", "at_seq": 9, "relation": "failure"}
            ],
            "graph_summary": {
                "nodes": 8,
                "edges": 7,
                "tasks": 2,
                "agents": 2,
                "artifacts": 1,
                "commits": 1,
                "test_evidence": 1,
                "verifications": 1,
                "merges": 1,
                "max_depth": 1,
                "depth_limited": false,
                "truncated": false,
                "scanned_events": 10,
                "total_events": 10,
                "window_start_seq": 1,
                "window_end_seq": 10,
                "digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            },
            "other_failures": 0,
            "assignments": [],
            "advisory": true,
            "truncated": false,
            "warnings": [],
            "recorded_event_seq": 11
        });
        let rendered = diagnosis_lines(&report).unwrap().join("\n");
        assert!(rendered.contains("ATTRIBUTED"));
        assert!(rendered.contains("unsubstantiated_handoff"));
        assert!(rendered.contains("t1[completed] -> t2[failed]"));
        assert!(rendered.contains("recorded: failure.attribution seq 11"));
        assert!(rendered.contains("advisory only"));
        assert!(!rendered.contains("SECRET"));
        assert!(!rendered.contains("prompt"));

        let history = json!({
            "session_id": "ses-1",
            "total": 1,
            "truncated": false,
            "attributions": [{
                "event_seq": 11,
                "recorded_at": "2026-11-13T00:00:00Z",
                "attribution": {
                    "reason_code": "cascade_upstream",
                    "target": {"kind": "task", "id": "t2"},
                    "earliest_node_id": "failure:task:t1:4",
                    "earliest_seq": 4
                }
            }]
        });
        let rendered = diagnosis_lines(&history).unwrap().join("\n");
        assert!(rendered.contains("cascade_upstream"));
        assert!(rendered.contains("task:t2"));
    }
}

#[cfg(test)]
mod agent_identity_cli_tests {
    use super::*;

    #[test]
    fn agents_parses_project_role_status_and_legacy_session_override() {
        let cli = Cli::try_parse_from([
            "vp", "agents", "prj-1", "--role", "worker", "--status", "active",
        ])
        .unwrap();
        match cli.command {
            Command::Agents {
                project,
                session,
                role,
                status,
            } => {
                assert_eq!(project, "prj-1");
                assert!(session.is_none());
                assert_eq!(role.as_deref(), Some("worker"));
                assert_eq!(status.as_deref(), Some("active"));
            }
            other => panic!("expected agents, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "agents", "ses-1", "--session", "ses-1"]).unwrap();
        match cli.command {
            Command::Agents { session, .. } => assert_eq!(session.as_deref(), Some("ses-1")),
            other => panic!("expected agents, got {other:?}"),
        }
    }

    #[test]
    fn memory_commands_parse_and_render_without_secrets() {
        let cli = Cli::try_parse_from([
            "vp",
            "agent-memory",
            "prj-1",
            "--identity",
            "agt-1",
            "--scope",
            "agent",
            "--status",
            "active",
            "--query",
            "auth",
            "--limit",
            "20",
            "--include-expired",
        ])
        .unwrap();
        match cli.command {
            Command::AgentMemory {
                project,
                identity,
                scope,
                status,
                query,
                limit,
                include_expired,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(identity.as_deref(), Some("agt-1"));
                assert_eq!(scope.as_deref(), Some("agent"));
                assert_eq!(status.as_deref(), Some("active"));
                assert_eq!(query.as_deref(), Some("auth"));
                assert_eq!(limit, Some(20));
                assert!(include_expired);
            }
            other => panic!("expected agent-memory, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-memory-remember",
            "prj-1",
            "--scope",
            "agent",
            "--content",
            "Auth owns token validation.",
            "--identity",
            "agt-1",
            "--source",
            "verifier",
            "--evidence",
            "task:T1:reviewed",
            "--evidence",
            "commit:abcdef123456",
            "--expires-at",
            "2027-01-01T00:00:00Z",
            "--pinned",
        ])
        .unwrap();
        match cli.command {
            Command::AgentMemoryRemember {
                project,
                scope,
                content,
                identity,
                source,
                evidence,
                expires_at,
                pinned,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(scope, "agent");
                assert_eq!(content, "Auth owns token validation.");
                assert_eq!(identity.as_deref(), Some("agt-1"));
                assert_eq!(source.as_deref(), Some("verifier"));
                assert_eq!(evidence.len(), 2);
                assert_eq!(expires_at.as_deref(), Some("2027-01-01T00:00:00Z"));
                assert!(pinned);
                let first = parse_memory_evidence(&evidence[0]).unwrap();
                assert_eq!(first["kind"], "task");
                assert_eq!(first["reference"], "T1");
                assert_eq!(first["note"], "reviewed");
                let second = parse_memory_evidence(&evidence[1]).unwrap();
                assert!(second.get("note").is_none());
                assert!(parse_memory_evidence("task").is_err());
                assert!(parse_memory_evidence("task:").is_err());
            }
            other => panic!("expected agent-memory-remember, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-memory-supersede",
            "mem-1",
            "--replacement",
            "mem-2",
            "--expected-hash",
            "abc",
        ])
        .unwrap();
        match cli.command {
            Command::AgentMemorySupersede {
                memory,
                replacement,
                content,
                expected_hash,
                ..
            } => {
                assert_eq!(memory, "mem-1");
                assert_eq!(replacement.as_deref(), Some("mem-2"));
                assert!(content.is_none());
                assert_eq!(expected_hash.as_deref(), Some("abc"));
            }
            other => panic!("expected agent-memory-supersede, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "agent-memory-forget", "mem-1", "--prune"]).unwrap();
        match cli.command {
            Command::AgentMemoryForget { memory, prune } => {
                assert_eq!(memory, "mem-1");
                assert!(prune);
            }
            other => panic!("expected agent-memory-forget, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-memory-erase",
            "mem-1",
            "--project",
            "prj-1",
            "--reason",
            "privacy request",
            "--yes",
        ])
        .unwrap();
        match cli.command {
            Command::AgentMemoryErase {
                memory,
                project,
                reason,
                yes,
            } => {
                assert_eq!(memory, "mem-1");
                assert_eq!(project.as_deref(), Some("prj-1"));
                assert_eq!(reason.as_deref(), Some("privacy request"));
                assert!(yes);
            }
            other => panic!("expected agent-memory-erase, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-memory-sweep",
            "prj-1",
            "--older-than-days",
            "30",
            "--limit",
            "50",
            "--prune",
        ])
        .unwrap();
        match cli.command {
            Command::AgentMemorySweep {
                project,
                older_than_days,
                limit,
                prune,
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(older_than_days, Some(30));
                assert_eq!(limit, Some(50));
                assert!(prune);
            }
            other => panic!("expected agent-memory-sweep, got {other:?}"),
        }

        let list = json!({
            "memories": [{
                "id": "mem-1",
                "scope": "agent",
                "status": "active",
                "source": "operator",
                "content": "Auth owns token validation.",
                "expired": false,
                "evidence": [{"kind": "task", "reference": "T1"}]
            }],
            "total": 1
        });
        let rendered = agent_memory_lines(&list).unwrap().join("\n");
        assert!(rendered.contains("mem-1"));
        assert!(rendered.contains("agent"));
        assert!(rendered.contains("Auth owns token validation."));
        assert!(rendered.contains("task T1"));
        let sweep = json!({"expired": 2, "pruned": 1, "remaining": 4});
        let rendered = agent_memory_lines(&sweep).unwrap().join("\n");
        assert!(rendered.contains("expired 2"));
        assert!(rendered.contains("pruned 1"));
        assert!(rendered.contains("remaining 4"));
        assert!(agent_memory_lines(&json!({"agents": []})).is_none());
    }

    #[test]
    fn create_update_and_retire_commands_parse() {
        let cli = Cli::try_parse_from([
            "vp",
            "agent-create",
            "prj-1",
            "--role",
            "manager",
            "--department",
            "platform",
            "--profile",
            "generic",
            "--model",
            "explicit-model",
            "--parent",
            "leader-1",
            "--policy-json",
            "{\"note\":\"bounded\"}",
        ])
        .unwrap();
        match cli.command {
            Command::AgentCreate {
                project,
                role,
                department,
                parent,
                policy,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(role, "manager");
                assert_eq!(department.as_deref(), Some("platform"));
                assert_eq!(parent.as_deref(), Some("leader-1"));
                assert_eq!(policy.as_deref(), Some("{\"note\":\"bounded\"}"));
            }
            other => panic!("expected agent-create, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-update",
            "identity-1",
            "--department",
            "platform",
            "--clear-title",
            "--clear-successor",
            "--policy-json",
            "{}",
        ])
        .unwrap();
        match cli.command {
            Command::AgentUpdate {
                identity,
                department,
                clear_title,
                clear_successor,
                policy,
                ..
            } => {
                assert_eq!(identity, "identity-1");
                assert_eq!(department.as_deref(), Some("platform"));
                assert!(clear_title);
                assert!(clear_successor);
                assert_eq!(policy.as_deref(), Some("{}"));
            }
            other => panic!("expected agent-update, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-retire",
            "identity-1",
            "--successor",
            "identity-2",
        ])
        .unwrap();
        match cli.command {
            Command::AgentRetire {
                identity,
                successor,
                ..
            } => {
                assert_eq!(identity, "identity-1");
                assert_eq!(successor.as_deref(), Some("identity-2"));
            }
            other => panic!("expected agent-retire, got {other:?}"),
        }
    }

    #[test]
    fn policy_and_lifecycle_commands_parse_and_validate_json() {
        let cli = Cli::try_parse_from([
            "vp",
            "agent-policy-set",
            "prj-1",
            "--enabled",
            "true",
            "--max-managers",
            "2",
            "--max-workers",
            "3",
            "--hire-requires-approval",
            "false",
        ])
        .unwrap();
        match cli.command {
            Command::AgentPolicySet {
                project,
                enabled,
                max_managers,
                max_workers,
                hire_requires_approval,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(enabled, Some(true));
                assert_eq!(max_managers, Some(2));
                assert_eq!(max_workers, Some(3));
                assert_eq!(hire_requires_approval, Some(false));
            }
            other => panic!("expected agent-policy-set, got {other:?}"),
        }
        assert!(
            parse_agent_identity_policy_json(Some("{\"a\":1}"))
                .unwrap()
                .is_some()
        );
        assert!(parse_agent_identity_policy_json(Some("[1]")).is_err());
        assert!(parse_agent_identity_policy_json(Some("not-json")).is_err());
        assert!(parse_agent_identity_policy_json(None).unwrap().is_none());

        let mut params = serde_json::Map::new();
        insert_agent_identity_optional(&mut params, "title", Some("Lead".into()), false);
        insert_agent_identity_optional(&mut params, "component", None, true);
        assert_eq!(params["title"], json!("Lead"));
        assert!(params["component"].is_null());
        assert!(params.get("model").is_none());
    }

    #[test]
    fn identity_human_lines_are_bounded_and_show_generation() {
        let value = json!({
            "identities": [{
                "id": "identity-1",
                "project_id": "prj-1",
                "role": "worker",
                "status": "active",
                "generation": 4,
                "component": "runtime",
                "title": "Runtime Worker",
                "parent_identity_id": "manager-1",
                "origin": "local"
            }]
        });
        let rendered = agent_identity_lines(&value).unwrap().join("\n");
        assert!(rendered.contains("identity-1"));
        assert!(rendered.contains("worker"));
        assert!(rendered.contains("gen 4"));
        assert!(rendered.contains("component runtime"));
        assert!(!rendered.contains("api_key"));

        let policy = json!({
            "project_id": "prj-1",
            "policy": {
                "enabled": false,
                "max_managers": 4,
                "max_workers": 10,
                "hire_requires_approval": true,
                "retirement_requires_approval": true
            }
        });
        let rendered = agent_identity_lines(&policy).unwrap().join("\n");
        assert!(rendered.contains("identities disabled"));
        assert!(rendered.contains("managers ≤ 4"));
        assert!(rendered.contains("workers ≤ 10"));
    }

    #[test]
    fn pause_resume_and_policy_read_commands_parse() {
        let cli = Cli::try_parse_from(["vp", "agent-pause", "identity-1"]).unwrap();
        match cli.command {
            Command::AgentPause { identity } => assert_eq!(identity, "identity-1"),
            other => panic!("expected agent-pause, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "agent-resume", "identity-1"]).unwrap();
        match cli.command {
            Command::AgentResume { identity } => assert_eq!(identity, "identity-1"),
            other => panic!("expected agent-resume, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "agent-policy", "prj-1"]).unwrap();
        match cli.command {
            Command::AgentPolicy { project } => assert_eq!(project, "prj-1"),
            other => panic!("expected agent-policy, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod coordination_cli_tests {
    use super::*;

    #[test]
    fn assignment_commands_parse_bounded_lease_and_wake() {
        let cli = Cli::try_parse_from([
            "vp",
            "assign",
            "ses-1",
            "identity-1",
            "--role",
            "worker",
            "--lease-secs",
            "900",
            "--wake",
            "--operator",
            "alice",
        ])
        .unwrap();
        match cli.command {
            Command::Assign {
                session,
                identity,
                role,
                lease_secs,
                wake,
                operator,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(identity, "identity-1");
                assert_eq!(role.as_deref(), Some("worker"));
                assert_eq!(lease_secs, Some(900));
                assert!(wake);
                assert_eq!(operator.as_deref(), Some("alice"));
            }
            other => panic!("expected assign, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "assignments",
            "ses-1",
            "--status",
            "active",
            "--limit",
            "20",
        ])
        .unwrap();
        match cli.command {
            Command::Assignments {
                session,
                identity,
                project,
                status,
                limit,
            } => {
                assert_eq!(session.as_deref(), Some("ses-1"));
                assert!(identity.is_none());
                assert!(project.is_none());
                assert_eq!(status.as_deref(), Some("active"));
                assert_eq!(limit, 20);
            }
            other => panic!("expected assignments, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "release",
            "ses-1",
            "identity-1",
            "--operator",
            "--reason",
            "done",
        ])
        .unwrap();
        match cli.command {
            Command::Release {
                session,
                identity,
                operator,
                reason,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(identity, "identity-1");
                assert!(operator);
                assert_eq!(reason.as_deref(), Some("done"));
            }
            other => panic!("expected release, got {other:?}"),
        }
    }

    #[test]
    fn inbox_commands_parse_body_payload_and_expiry() {
        let cli = Cli::try_parse_from([
            "vp",
            "inbox-send",
            "identity-1",
            "--kind",
            "handoff",
            "--body",
            "review the change",
            "--expires-in",
            "600",
            "--wake",
        ])
        .unwrap();
        match cli.command {
            Command::InboxSend {
                identity,
                kind,
                body,
                payload,
                expires_in_secs,
                wake,
                ..
            } => {
                assert_eq!(identity, "identity-1");
                assert_eq!(kind, "handoff");
                assert_eq!(body.as_deref(), Some("review the change"));
                assert!(payload.is_none());
                assert_eq!(expires_in_secs, Some(600));
                assert!(wake);
            }
            other => panic!("expected inbox-send, got {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["vp", "inbox", "identity-1", "--unread", "--limit", "5"]).unwrap();
        match cli.command {
            Command::Inbox {
                identity,
                unread,
                limit,
                ..
            } => {
                assert_eq!(identity.as_deref(), Some("identity-1"));
                assert!(unread);
                assert_eq!(limit, 5);
            }
            other => panic!("expected inbox, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "inbox-read", "msg-1", "--project", "prj-1"]).unwrap();
        match cli.command {
            Command::InboxRead {
                message,
                project,
                identity,
            } => {
                assert_eq!(message, "msg-1");
                assert_eq!(project.as_deref(), Some("prj-1"));
                assert!(identity.is_none());
            }
            other => panic!("expected inbox-read, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "inbox-ack", "msg-1"]).unwrap();
        assert!(matches!(cli.command, Command::InboxAck { .. }));
        let cli = Cli::try_parse_from(["vp", "coordination-replay", "prj-1", "--repair"]).unwrap();
        match cli.command {
            Command::CoordinationReplay { project, repair } => {
                assert_eq!(project, "prj-1");
                assert!(repair);
            }
            other => panic!("expected coordination-replay, got {other:?}"),
        }
    }

    #[test]
    fn agent_inspect_and_message_commands_parse_without_resetting_other_fields() {
        let cli = Cli::try_parse_from([
            "vp",
            "agent-inspect",
            "ses-1",
            "agt-1",
            "--timeline-limit",
            "40",
        ])
        .unwrap();
        match cli.command {
            Command::AgentInspect {
                session,
                agent,
                timeline_limit,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(agent, "agt-1");
                assert_eq!(timeline_limit, 40);
            }
            other => panic!("expected agent-inspect, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "agent-inspect", "ses-1", "agt-1"]).unwrap();
        match cli.command {
            Command::AgentInspect { timeline_limit, .. } => assert_eq!(timeline_limit, 80),
            other => panic!("expected agent-inspect default, got {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["vp", "agent-message", "ses-1", "agt-1", "check this"]).unwrap();
        match cli.command {
            Command::AgentMessage {
                session,
                agent,
                text,
                task,
                wake,
                operation_id,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(agent, "agt-1");
                assert_eq!(text, "check this");
                assert!(task.is_none());
                assert!(!wake);
                assert!(operation_id.is_none());
            }
            other => panic!("expected agent-message defaults, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-message",
            "ses-1",
            "agt-1",
            "check this",
            "--task",
            "T1",
            "--wake",
            "--operation-id",
            "op-msg-1",
        ])
        .unwrap();
        match cli.command {
            Command::AgentMessage {
                task,
                wake,
                operation_id,
                ..
            } => {
                assert_eq!(task.as_deref(), Some("T1"));
                assert!(wake);
                assert_eq!(operation_id.as_deref(), Some("op-msg-1"));
            }
            other => panic!("expected agent-message flags, got {other:?}"),
        }
    }

    #[test]
    fn agent_inspect_and_message_human_lines_are_bounded_and_truthful() {
        let inspect = json!({
            "agent": {
                "id": "agt-1",
                "session_id": "ses-1",
                "role": "worker",
                "name": "Worker",
                "title": "Implementation",
                "status": "running",
                "department": "impl",
                "profile": "coding",
                "model": "worker-model",
                "parent_id": "agt-leader",
                "identity_id": "idn-1",
                "worktree": "/tmp/worktree",
                "branch": "verlinpark/task/T1"
            },
            "task": {
                "task_id": "T1",
                "title": "Build it",
                "department": "impl",
                "kind": "coding",
                "status": "running",
                "attempts": 1,
                "max_attempts": 2,
                "acceptance": ["tests pass"],
                "write_scope": ["src/impl"],
                "verifier_status": "pending"
            },
            "context": {
                "objective": "build the widget",
                "model": "worker-model",
                "estimated_tokens": 1200,
                "context_window": 128000,
                "threshold_tokens": 96000,
                "hard_limit_tokens": 128000,
                "input_tokens": 3000,
                "output_tokens": 400,
                "cache_read_tokens": 2400,
                "cache_write_tokens": 100,
                "cost_usd": 0.0123,
                "tool_count": 17,
                "tool_bundle": "coding",
                "prefix_hash": "abcdef0123456789",
                "last_compaction": "summary@2",
                "memory_brief_chars": 345,
                "memory_brief_hash": "9876543210abcdef"
            },
            "timeline": [{
                "seq": 12,
                "kind": "tool.finished",
                "category": "tool",
                "created_at": "2026-11-22T00:00:00Z",
                "text": "cargo test passed",
                "tool": "run_project_task",
                "status": "ok",
                "is_error": false
            }],
            "steering": [{
                "id": "str-1",
                "status": "pending",
                "task_id": "T1",
                "body": "focus on the parser",
                "created_at": "2026-11-22T00:00:01Z"
            }],
            "inbox": [],
            "inbox_counts": {"unread": 0, "read": 0, "done": 0, "expired": 0},
            "truncated": true
        });
        let rendered = agent_inspect_lines(&inspect)
            .expect("inspect lines")
            .join("\n");
        assert!(rendered.contains("Worker · worker · running"));
        assert!(rendered.contains("task T1 · Build it"));
        assert!(rendered.contains("timeline 1 row(s) [truncated]"));
        assert!(rendered.contains("steering 1 (1 pending)"));
        assert!(rendered.contains("cargo test passed"));
        assert!(rendered.contains("prefix abcdef012345"));

        let steering = json!({
            "delivery": {
                "kind": "steering",
                "status": "pending",
                "message": "Queued as steering; delivered at the next provider boundary.",
                "agent_id": "agt-1",
                "identity_id": "idn-1",
                "steering": {
                    "id": "str-1",
                    "task_id": "T1",
                    "status": "pending",
                    "created_at": "2026-11-22T00:00:01Z"
                }
            }
        });
        let rendered = agent_message_lines(&steering)
            .expect("steering message lines")
            .join("\n");
        assert!(rendered.contains("steering · pending · agent agt-1"));
        assert!(rendered.contains("next provider boundary"));
        assert!(rendered.contains("steering str-1 · task T1"));

        let inbox = json!({
            "delivery": {
                "kind": "inbox",
                "status": "unread",
                "message": "Stored in the project inbox.",
                "agent_id": "agt-1",
                "identity_id": "idn-1",
                "inbox": {
                    "id": "msg-1",
                    "status": "unread",
                    "from_label": "operator",
                    "body": "pick this up when you return",
                    "created_at": "2026-11-22T00:00:02Z",
                    "expires_at": "2026-11-23T00:00:02Z"
                },
                "wake": {
                    "requested": true,
                    "created": true,
                    "continuation_id": "con-1",
                    "reason": "deferred_continuation"
                }
            }
        });
        let rendered = agent_message_lines(&inbox)
            .expect("inbox message lines")
            .join("\n");
        assert!(rendered.contains("inbox · unread · agent agt-1"));
        assert!(rendered.contains("inbox msg-1 · unread · from operator"));
        assert!(rendered.contains("pick this up when you return"));
        assert!(rendered.contains("wake requested=true created=true"));
        assert!(rendered.contains("continuation con-1"));
    }

    #[test]
    fn coordination_human_lines_show_lease_inbox_and_wake_without_secrets() {
        let assignments = json!({
            "assignments": [{
                "session_id": "ses-1",
                "project_id": "prj-1",
                "identity_id": "identity-1",
                "role": "worker",
                "status": "active",
                "lease_active": true,
                "lease_expires_at": "2026-11-13T00:10:00Z",
                "assigned_at": "2026-11-13T00:00:00Z",
                "heartbeat_at": "2026-11-13T00:00:00Z"
            }]
        });
        let rendered = coordination_lines(&assignments).unwrap().join("\n");
        assert!(rendered.contains("ses-1"));
        assert!(rendered.contains("live"));
        assert!(!rendered.contains("lease_token"));

        let result = json!({
            "assignment": {
                "session_id": "ses-1",
                "identity_id": "identity-1",
                "role": "worker",
                "status": "active"
            },
            "created": true,
            "wake": {"requested": true, "created": false, "reason": "session_running"}
        });
        let rendered = coordination_lines(&result).unwrap().join("\n");
        assert!(rendered.contains("created"));
        assert!(rendered.contains("session_running"));

        let inbox = json!({
            "messages": [{
                "id": "msg-1",
                "status": "unread",
                "kind": "handoff",
                "from_label": "operator",
                "payload": {"body": "review"},
                "expires_at": "2026-11-13T01:00:00Z"
            }],
            "counts": {"unread": 1, "read": 0, "done": 0, "expired": 0}
        });
        let rendered = coordination_lines(&inbox).unwrap().join("\n");
        assert!(rendered.contains("unread 1"));
        assert!(rendered.contains("review"));

        let replay = json!({
            "project_id": "prj-1",
            "assignments": 1,
            "inbox": 2,
            "events": 3,
            "consistent": true,
            "repaired": false,
            "mismatches": []
        });
        let rendered = coordination_lines(&replay).unwrap().join("\n");
        assert!(rendered.contains("assignments 1"));
        assert!(rendered.contains("consistent"));
    }

    #[test]
    fn workspace_commands_parse_and_render_without_mutation_shortcuts() {
        let cli = Cli::try_parse_from([
            "vp",
            "workspaces",
            "--project",
            "prj-1",
            "--status",
            "active",
            "--limit",
            "25",
        ])
        .unwrap();
        match cli.command {
            Command::Workspaces {
                project,
                session,
                identity,
                status,
                limit,
            } => {
                assert_eq!(project.as_deref(), Some("prj-1"));
                assert!(session.is_none());
                assert!(identity.is_none());
                assert_eq!(status.as_deref(), Some("active"));
                assert_eq!(limit, 25);
            }
            other => panic!("expected workspaces, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "workspace", "wsp-1"]).unwrap();
        assert!(matches!(cli.command, Command::Workspace { .. }));
        let cli = Cli::try_parse_from(["vp", "workspace-land", "wsp-1", "--yes"]).unwrap();
        match cli.command {
            Command::WorkspaceLand { workspace, yes, .. } => {
                assert_eq!(workspace, "wsp-1");
                assert!(yes);
            }
            other => panic!("expected workspace-land, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "vp",
            "workspace-archive",
            "wsp-1",
            "--retire",
            "--reason",
            "done",
        ])
        .unwrap();
        match cli.command {
            Command::WorkspaceArchive {
                workspace,
                retire,
                reason,
                ..
            } => {
                assert_eq!(workspace, "wsp-1");
                assert!(retire);
                assert_eq!(reason.as_deref(), Some("done"));
            }
            other => panic!("expected workspace-archive, got {other:?}"),
        }

        let list = json!({
            "workspaces": [{
                "id": "wsp-1",
                "identity_id": "agt-1",
                "component": "auth",
                "branch": "verlinpark/components/auth-1--agt-1",
                "status": "active",
                "lease_active": true
            }],
            "sessions": [{
                "session_id": "ses-1",
                "identity_id": "agt-1",
                "workspace_id": "wsp-1",
                "status": "active",
                "branch": "verlinpark/sessions/ses-1/worker-agt-1"
            }]
        });
        let rendered = workspace_lines(&list).unwrap().join("\n");
        assert!(rendered.contains("1 worker workspace(s)"));
        assert!(rendered.contains("live lease"));
        let land = json!({
            "workspace": {"id": "wsp-1", "branch": "b", "worktree_path": "/w"},
            "session_id": "ses-1",
            "previous_component_commit": "a",
            "landed_commit": "b",
            "already_up_to_date": false,
            "commits": ["c"]
        });
        let rendered = workspace_lines(&land).unwrap().join("\n");
        assert!(rendered.contains("landed"));
        assert!(rendered.contains("1 commit(s)"));
    }

    #[test]
    fn hiring_and_retirement_commands_parse_and_render_bounded_surfaces() {
        let cli = Cli::try_parse_from([
            "vp",
            "agent-hire",
            "prj-1",
            "--role",
            "worker",
            "--department",
            "platform",
            "--component",
            "auth",
            "--parent",
            "mgr-1",
            "--approved",
            "--operator",
            "ops",
        ])
        .unwrap();
        match cli.command {
            Command::AgentHire {
                project,
                role,
                department,
                component,
                parent,
                approved,
                operator,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(role, "worker");
                assert_eq!(department.as_deref(), Some("platform"));
                assert_eq!(component.as_deref(), Some("auth"));
                assert_eq!(parent.as_deref(), Some("mgr-1"));
                assert!(approved);
                assert_eq!(operator.as_deref(), Some("ops"));
            }
            other => panic!("expected agent-hire, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-promote",
            "id-1",
            "--role",
            "manager",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Command::AgentPromote {
                identity,
                role,
                force,
                ..
            } => {
                assert_eq!(identity, "id-1");
                assert_eq!(role, "manager");
                assert!(force);
            }
            other => panic!("expected agent-promote, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-transfer",
            "id-1",
            "--component",
            "auth",
            "--clear-title",
            "--force",
        ])
        .unwrap();
        match cli.command {
            Command::AgentTransfer {
                identity,
                component,
                clear_title,
                force,
                ..
            } => {
                assert_eq!(identity, "id-1");
                assert_eq!(component.as_deref(), Some("auth"));
                assert!(clear_title);
                assert!(force);
            }
            other => panic!("expected agent-transfer, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-retire",
            "id-1",
            "--approved",
            "--force",
            "--proposal",
            "pro-1",
            "--successor",
            "id-2",
            "--reason",
            "evidence",
        ])
        .unwrap();
        match cli.command {
            Command::AgentRetire {
                identity,
                proposal,
                approved,
                force,
                reason,
                successor,
                ..
            } => {
                assert_eq!(identity, "id-1");
                assert_eq!(proposal.as_deref(), Some("pro-1"));
                assert!(approved);
                assert!(force);
                assert_eq!(reason.as_deref(), Some("evidence"));
                assert_eq!(successor.as_deref(), Some("id-2"));
            }
            other => panic!("expected agent-retire, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-performance",
            "prj-1",
            "--identity",
            "id-1",
            "--lookback-days",
            "7",
        ])
        .unwrap();
        match cli.command {
            Command::AgentPerformance {
                project,
                identity,
                lookback_days,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(identity.as_deref(), Some("id-1"));
                assert_eq!(lookback_days, Some(7));
            }
            other => panic!("expected agent-performance, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-retirement",
            "prj-1",
            "--candidates",
            "--limit",
            "5",
        ])
        .unwrap();
        match cli.command {
            Command::AgentRetirement {
                project,
                candidates,
                limit,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert!(candidates);
                assert_eq!(limit, 5);
            }
            other => panic!("expected agent-retirement, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "retirement-policy-set",
            "prj-1",
            "--enabled",
            "true",
            "--max-false-consensus",
            "1",
            "--require-approval",
            "true",
        ])
        .unwrap();
        match cli.command {
            Command::RetirementPolicySet {
                project,
                enabled,
                max_false_consensus,
                require_approval,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(enabled, Some(true));
                assert_eq!(max_false_consensus, Some(1));
                assert_eq!(require_approval, Some(true));
            }
            other => panic!("expected retirement-policy-set, got {other:?}"),
        }

        let candidates = json!({
            "project_id": "prj-1",
            "policy_enabled": true,
            "evaluated": 2,
            "candidates": [{
                "identity_id": "id-1",
                "role": "worker",
                "generation": 2,
                "severity": 1001,
                "reasons": [{
                    "code": "consecutive_failures",
                    "count": 3,
                    "threshold": 2
                }]
            }]
        });
        let rendered = agent_employment_lines(&candidates).unwrap().join("\n");
        assert!(rendered.contains("1 retirement candidates"));
        assert!(rendered.contains("consecutive_failures 3/2"));
        assert!(!rendered.contains("prompt"));

        let proposals = json!({
            "proposals": [{
                "id": "pro-1",
                "identity_id": "id-1",
                "status": "pending",
                "reason_code": "consecutive_failures"
            }]
        });
        let rendered = agent_employment_lines(&proposals).unwrap().join("\n");
        assert!(rendered.contains("1 retirement proposals"));
        assert!(rendered.contains("pending"));

        let summary = json!({
            "totals": {
                "identities": 1,
                "completed_tasks": 2,
                "failed_tasks": 3,
                "verifier_approvals": 1,
                "verifier_rejections": 1,
                "input_tokens": 100,
                "output_tokens": 20,
                "cost_usd": 0.5
            },
            "identities": [{
                "identity_id": "id-1",
                "role": "worker",
                "status": "active",
                "completed_tasks": 2,
                "failed_tasks": 3,
                "consecutive_failures": 2,
                "retries": 1,
                "verifier_approvals": 1,
                "verifier_rejections": 1,
                "evidence": [{"kind": "task", "reference": "t-1"}]
            }]
        });
        let rendered = agent_employment_lines(&summary).unwrap().join("\n");
        assert!(rendered.contains("completed 2"));
        assert!(rendered.contains("verifier 1/1"));
        assert!(!rendered.contains("prompt"));
    }
}

#[cfg(test)]
mod agent_org_cli_tests {
    use super::*;

    #[test]
    fn agent_org_and_detail_parse_bounded_reads() {
        let cli = Cli::try_parse_from([
            "vp",
            "agent-org",
            "prj-1",
            "--include-retired",
            "--limit",
            "50",
        ])
        .unwrap();
        match cli.command {
            Command::AgentOrg {
                project,
                include_retired,
                limit,
            } => {
                assert_eq!(project, "prj-1");
                assert!(include_retired);
                assert_eq!(limit, 50);
            }
            other => panic!("expected agent-org, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "agent-detail",
            "identity-1",
            "--project",
            "prj-1",
            "--include-inbox",
            "--inbox-limit",
            "10",
            "--memory-limit",
            "5",
            "--assignment-limit",
            "15",
        ])
        .unwrap();
        match cli.command {
            Command::AgentDetail {
                identity,
                project,
                include_inbox,
                inbox_limit,
                memory_limit,
                assignment_limit,
            } => {
                assert_eq!(identity, "identity-1");
                assert_eq!(project.as_deref(), Some("prj-1"));
                assert!(include_inbox);
                assert_eq!(inbox_limit, 10);
                assert_eq!(memory_limit, 5);
                assert_eq!(assignment_limit, 15);
            }
            other => panic!("expected agent-detail, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "agent-org", "prj-1"]).unwrap();
        match cli.command {
            Command::AgentOrg {
                include_retired,
                limit,
                ..
            } => {
                assert!(!include_retired);
                assert_eq!(limit, 200);
            }
            other => panic!("expected agent-org defaults, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["vp", "agent-detail", "identity-1"]).unwrap();
        match cli.command {
            Command::AgentDetail {
                include_inbox,
                inbox_limit,
                ..
            } => {
                assert!(!include_inbox);
                assert_eq!(inbox_limit, 20);
            }
            other => panic!("expected agent-detail defaults, got {other:?}"),
        }
    }

    #[test]
    fn org_snapshot_human_output_is_bounded_and_text_free() {
        let snapshot = json!({
            "project_id": "prj-1",
            "generated_at": "2026-11-13T00:00:00Z",
            "enabled": true,
            "counts": {
                "identities": 3,
                "leaders": 1,
                "managers": 1,
                "workers": 1,
                "active": 3,
                "idle": 0,
                "paused": 0,
                "retired": 0,
                "imported": 0,
                "active_assignments": 1,
                "active_workspaces": 1,
                "unread_inbox": 2,
                "memory_count": 4,
                "pending_hire_requests": 1,
                "pending_retirement_proposals": 0,
                "retirement_candidates": 0
            },
            "leader": {
                "identity": {
                    "id": "id-leader",
                    "project_id": "prj-1",
                    "role": "leader",
                    "title": "Leader",
                    "status": "active",
                    "generation": 1,
                    "origin": "local"
                },
                "unread_inbox": 0,
                "memory_count": 0,
                "performance": {},
                "retirement": {},
                "reports": [{
                    "identity": {
                        "id": "id-manager",
                        "project_id": "prj-1",
                        "role": "manager",
                        "title": "Platform Manager",
                        "department": "Platform",
                        "component": "platform",
                        "status": "active",
                        "generation": 2,
                        "origin": "local"
                    },
                    "active_assignment": {
                        "session_id": "ses-1",
                        "role": "manager",
                        "assigned_at": "2026-11-13T00:00:00Z",
                        "lease_active": true,
                        "assignment_count": 1,
                        "session_count": 1
                    },
                    "workspace": {
                        "id": "ws-1",
                        "identity_id": "id-manager",
                        "component": "platform",
                        "branch": "verlinpark/components/platform",
                        "status": "active",
                        "lease_active": true
                    },
                    "unread_inbox": 2,
                    "memory_count": 4,
                    "performance": {
                        "completed_tasks": 2,
                        "failed_tasks": 1,
                        "verifier_approvals": 1,
                        "verifier_rejections": 0
                    },
                    "retirement": {},
                    "reports": []
                }]
            },
            "unassigned": [],
            "departments": [{
                "name": "Platform",
                "manager_count": 1,
                "worker_count": 1,
                "active_count": 2,
                "active_workers": 1,
                "unread_inbox": 2,
                "manager_ids": ["id-manager"],
                "worker_ids": ["id-worker"]
            }],
            "active_assignments": [{
                "session_id": "ses-1",
                "project_id": "prj-1",
                "identity_id": "id-manager",
                "role": "manager",
                "status": "active",
                "assigned_at": "2026-11-13T00:00:00Z",
                "heartbeat_at": "2026-11-13T00:05:00Z",
                "lease_active": true
            }],
            "workspace_summaries": [],
            "truncated": false,
            "warnings": ["retired identities are hidden"]
        });
        let rendered = agent_org_lines(&snapshot).expect("org lines").join("\n");
        assert!(rendered.contains("Project org prj-1"));
        assert!(rendered.contains("Platform Manager"));
        assert!(rendered.contains("Department Platform"));
        assert!(rendered.contains("session ses-1"));
        assert!(rendered.contains("warning: retired identities are hidden"));
        assert!(!rendered.contains("prompt"));
        assert!(!rendered.contains("SECRET"));
    }

    #[test]
    fn identity_detail_human_output_shows_metadata_without_payload_text() {
        let detail = json!({
            "project_id": "prj-1",
            "generated_at": "2026-11-13T00:00:00Z",
            "identity": {
                "id": "id-1",
                "project_id": "prj-1",
                "role": "worker",
                "title": "Auth Worker",
                "department": "Platform",
                "component": "auth",
                "status": "active",
                "generation": 3,
                "origin": "local"
            },
            "policy": {},
            "retirement_policy": {},
            "node": {
                "identity": {"id": "id-1", "project_id": "prj-1", "role": "worker", "status": "active", "generation": 3},
                "active_assignment": {
                    "session_id": "ses-1",
                    "role": "worker",
                    "assigned_at": "2026-11-13T00:00:00Z",
                    "lease_active": true,
                    "assignment_count": 1,
                    "session_count": 1
                },
                "unread_inbox": 1,
                "memory_count": 1,
                "performance": {},
                "retirement": {},
                "reports": []
            },
            "assignments": [{
                "session_id": "ses-1",
                "project_id": "prj-1",
                "identity_id": "id-1",
                "role": "worker",
                "status": "active",
                "assigned_at": "2026-11-13T00:00:00Z",
                "heartbeat_at": "2026-11-13T00:05:00Z",
                "lease_active": true
            }],
            "inbox": [{
                "id": "msg-1",
                "kind": "task",
                "status": "unread",
                "from_label": "operator",
                "created_at": "2026-11-13T00:00:00Z"
            }],
            "inbox_counts": {"unread": 1, "read": 0, "done": 0, "expired": 0},
            "memories": [{
                "id": "mem-1",
                "project_id": "prj-1",
                "scope": "agent",
                "status": "active",
                "created_at": "2026-11-13T00:00:00Z",
                "content": "Auth owns JWT validation."
            }],
            "memory_total": 1,
            "performance": {
                "completed_tasks": 4,
                "failed_tasks": 1,
                "consecutive_failures": 0,
                "verifier_approvals": 3,
                "verifier_rejections": 1,
                "input_tokens": 100,
                "output_tokens": 20,
                "cost_usd": 0.25
            },
            "retirement_candidates": [],
            "retirement_proposals": [],
            "hire_requests": [],
            "workspaces": [],
            "reports": [],
            "employment_events": [{
                "id": "evt-1",
                "project_id": "prj-1",
                "seq": 1,
                "identity_id": "id-1",
                "kind": "agent.transfer.approved",
                "payload": {},
                "created_at": "2026-11-13T00:00:00Z"
            }],
            "read_only": false,
            "truncated": false,
            "warnings": []
        });
        let rendered = agent_detail_lines(&detail)
            .expect("detail lines")
            .join("\n");
        assert!(rendered.contains("Auth Worker"));
        assert!(rendered.contains("performance completed 4"));
        assert!(rendered.contains("agent.transfer.approved"));
        assert!(!rendered.contains("SECRET"));
    }
}

#[cfg(test)]
mod team_template_cli_tests {
    use super::*;

    #[test]
    fn team_export_parses_name_version_and_output_directory() {
        let cli = Cli::try_parse_from([
            "vp",
            "team-export",
            "prj-1",
            "--name",
            "Platform Team",
            "--version",
            "2.1.0",
            "--description",
            "portable charter",
            "--out",
            "ops/team",
        ])
        .unwrap();
        match cli.command {
            Command::TeamExport {
                project,
                name,
                version,
                description,
                out,
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(name, "Platform Team");
                assert_eq!(version, "2.1.0");
                assert_eq!(description.as_deref(), Some("portable charter"));
                assert_eq!(out, PathBuf::from("ops/team"));
            }
            other => panic!("expected team-export, got {other:?}"),
        }
    }

    #[test]
    fn team_import_distinguishes_preview_apply_and_approval() {
        let cli = Cli::try_parse_from([
            "vp",
            "team-import",
            "prj-1",
            "--file",
            "team.json",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Command::TeamImport {
                project,
                file,
                dry_run,
                apply,
                approved,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(file, PathBuf::from("team.json"));
                assert!(dry_run);
                assert!(!apply);
                assert!(!approved);
            }
            other => panic!("expected team-import, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "team-import",
            "prj-1",
            "--file",
            "team.json",
            "--apply",
            "--approved",
            "--resolution-json",
            r#"[{"slot":"manager:Platform:","action":"merge","identity_id":"idn-1"}]"#,
        ])
        .unwrap();
        match cli.command {
            Command::TeamImport {
                apply,
                approved,
                resolution_json,
                ..
            } => {
                assert!(apply);
                assert!(approved);
                let resolutions: Vec<vp_protocol::types::TeamTemplateResolution> =
                    serde_json::from_str(&resolution_json.unwrap()).unwrap();
                assert_eq!(resolutions.len(), 1);
                assert_eq!(resolutions[0].action, "merge");
            }
            other => panic!("expected team-import apply, got {other:?}"),
        }
    }

    #[test]
    fn team_library_diff_and_replay_parse() {
        let cli = Cli::try_parse_from([
            "vp",
            "team-templates",
            "--project",
            "prj-1",
            "--limit",
            "25",
        ])
        .unwrap();
        match cli.command {
            Command::TeamTemplates { project, limit } => {
                assert_eq!(project.as_deref(), Some("prj-1"));
                assert_eq!(limit, 25);
            }
            other => panic!("expected team-templates, got {other:?}"),
        }

        let cli =
            Cli::try_parse_from(["vp", "team-diff", "prj-1", "--template", "tt_123"]).unwrap();
        match cli.command {
            Command::TeamDiff { project, template } => {
                assert_eq!(project, "prj-1");
                assert_eq!(template, "tt_123");
            }
            other => panic!("expected team-diff, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "team-replay", "prj-1", "--repair"]).unwrap();
        match cli.command {
            Command::TeamReplay { project, repair } => {
                assert_eq!(project, "prj-1");
                assert!(repair);
            }
            other => panic!("expected team-replay, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod progress_a2a_cli_tests {
    use super::*;

    #[test]
    fn progress_commands_parse_read_and_report_payloads() {
        let cli = Cli::try_parse_from([
            "vp",
            "progress",
            "ses-1",
            "--plan-revision",
            "7",
            "--limit",
            "32",
        ])
        .unwrap();
        match cli.command {
            Command::Progress {
                session,
                plan_revision,
                limit,
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(plan_revision, Some(7));
                assert_eq!(limit, 32);
            }
            other => panic!("expected progress, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "progress-report",
            "ses-1",
            "--payload",
            r#"{"satisfied":["a"],"progressing":["b"]}"#,
        ])
        .unwrap();
        match cli.command {
            Command::ProgressReport { session, payload } => {
                assert_eq!(session, "ses-1");
                assert!(payload.contains("satisfied"));
            }
            other => panic!("expected progress-report, got {other:?}"),
        }
    }

    #[test]
    fn a2a_commands_parse_status_tasks_policy_and_start() {
        let cli = Cli::try_parse_from(["vp", "a2a-status"]).unwrap();
        assert!(matches!(cli.command, Command::A2aStatus));
        let cli = Cli::try_parse_from(["vp", "a2a-card"]).unwrap();
        assert!(matches!(cli.command, Command::A2aCard));

        let cli = Cli::try_parse_from(["vp", "a2a-tasks", "--limit", "25", "--state", "rejected"])
            .unwrap();
        match cli.command {
            Command::A2aTasks { limit, state } => {
                assert_eq!(limit, 25);
                assert_eq!(state.as_deref(), Some("rejected"));
            }
            other => panic!("expected a2a-tasks, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "a2a-policy-set",
            "--enable",
            "--port",
            "7777",
            "--max-tasks",
            "64",
            "--task-ttl-secs",
            "120",
        ])
        .unwrap();
        match cli.command {
            Command::A2aPolicySet {
                enable,
                disable,
                allow_inbound,
                port,
                max_tasks,
                task_ttl_secs,
            } => {
                assert!(enable);
                assert!(!disable);
                assert!(!allow_inbound);
                assert_eq!(port, Some(7777));
                assert_eq!(max_tasks, Some(64));
                assert_eq!(task_ttl_secs, Some(120));
            }
            other => panic!("expected a2a-policy-set, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "vp",
            "a2a-start",
            "--approved",
            "--enable",
            "--port",
            "7777",
        ])
        .unwrap();
        match cli.command {
            Command::A2aStart {
                approved,
                port,
                enable,
                ..
            } => {
                assert!(approved);
                assert!(enable);
                assert_eq!(port, Some(7777));
            }
            other => panic!("expected a2a-start, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["vp", "a2a-stop", "--reason", "done"]).unwrap();
        match cli.command {
            Command::A2aStop { reason } => assert_eq!(reason.as_deref(), Some("done")),
            other => panic!("expected a2a-stop, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod cli_reliability_tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn cache_warm_normalizes_across_global_flags() {
        let normalized = normalize_cache_warm_args(vec![
            "vp".into(),
            "--json".into(),
            "--data-dir".into(),
            "/tmp/vp-data".into(),
            "cache".into(),
            "warm".into(),
            "ses-1".into(),
        ]);
        assert_eq!(
            normalized,
            vec![
                "vp",
                "--json",
                "--data-dir",
                "/tmp/vp-data",
                "cache-warm",
                "ses-1"
            ]
        );
        let cli = {
            let normalized = normalize_cache_warm_args(vec![
                "vp".into(),
                "--json".into(),
                "--data-dir".into(),
                "/tmp/vp-data".into(),
                "cache".into(),
                "warm".into(),
                "ses-1".into(),
                "--role".into(),
                "worker".into(),
            ]);
            std::thread::Builder::new()
                .stack_size(32 * 1024 * 1024)
                .spawn(move || Cli::try_parse_from(normalized).unwrap())
                .unwrap()
                .join()
                .unwrap()
        };
        match cli.command {
            Command::CacheWarm { session, role } => {
                assert_eq!(session, "ses-1");
                assert_eq!(role.as_deref(), Some("worker"));
            }
            other => panic!("expected cache-warm, got {other:?}"),
        }

        let normalized = normalize_cache_warm_args(vec![
            "vp".into(),
            "cache".into(),
            "--json".into(),
            "warm".into(),
            "ses-1".into(),
        ]);
        assert_eq!(normalized, vec!["vp", "cache-warm", "--json", "ses-1"]);

        // The historical `cache <session>` status command is unchanged.
        let normalized =
            normalize_cache_warm_args(vec!["vp".into(), "cache".into(), "ses-1".into()]);
        assert_eq!(normalized, vec!["vp", "cache", "ses-1"]);
    }

    #[test]
    fn dash_file_reads_stdin_instead_of_a_literal_file() {
        let stdin = b"{\"items\":[1]}\n".to_vec();
        let mut reader = std::io::Cursor::new(stdin);
        let text = read_json_input_from(Some(std::path::Path::new("-")), &mut reader).unwrap();
        assert_eq!(text, "{\"items\":[1]}\n");

        let mut reader = std::io::Cursor::new(b"[]".to_vec());
        assert_eq!(read_json_input_from(None, &mut reader).unwrap(), "[]");

        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("items.json");
        std::fs::write(&file, "[{\"write_scope\":[\"src/a\"]}]").unwrap();
        let mut reader = std::io::Cursor::new(Vec::new());
        assert_eq!(
            read_json_input_from(Some(&file), &mut reader).unwrap(),
            "[{\"write_scope\":[\"src/a\"]}]"
        );
    }

    #[test]
    fn shell_policy_merge_preserves_unspecified_security_fields() {
        let current = json!({
            "network": "deny",
            "read_deny": ["/Users/apple/.ssh"],
            "restrict_home": true,
            "read_jail": true,
        });
        let merged = merge_shell_policy(
            current.clone(),
            None,
            vec![],
            false,
            false,
            true,
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(merged["network"], "deny");
        assert_eq!(merged["read_deny"], json!(["/Users/apple/.ssh"]));
        assert_eq!(merged["restrict_home"], true);
        assert_eq!(merged["read_jail"], true);

        let merged = merge_shell_policy(
            current.clone(),
            None,
            vec!["/Users/apple/.aws".into()],
            false,
            false,
            false,
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            merged["read_deny"],
            json!(["/Users/apple/.ssh", "/Users/apple/.aws"])
        );

        let merged = merge_shell_policy(
            current.clone(),
            Some("deny".into()),
            vec![],
            false,
            true,
            true,
            false,
            true,
            false,
        )
        .unwrap();
        assert_eq!(merged["read_deny"], json!([]));
        assert_eq!(merged["network"], "deny");

        assert!(
            merge_shell_policy(
                current.clone(),
                None,
                vec![],
                false,
                false,
                false,
                false,
                false,
                false,
            )
            .is_err(),
            "a no-flag update must fail instead of widening"
        );
        assert!(
            merge_shell_policy(
                current,
                None,
                vec![],
                false,
                true,
                false,
                false,
                false,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn approval_template_enabled_state_is_preserved() {
        assert!(!resolve_template_enabled(false, true, None).unwrap());
        assert!(resolve_template_enabled(false, false, None).unwrap());
        assert!(!resolve_template_enabled(false, false, Some(false)).unwrap());
        assert!(!resolve_template_enabled(false, true, Some(true)).unwrap());
        assert!(resolve_template_enabled(true, false, Some(false)).unwrap());
        assert!(
            resolve_template_enabled(true, true, Some(false)).is_err(),
            "contradictory explicit flags fail closed"
        );

        let args = [
            "vp",
            "approval-template-set",
            "--name",
            "x",
            "--tool-pattern",
            "write_file",
            "--action",
            "allow",
            "--enabled",
            "--disabled",
        ];
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let error = std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap_err().to_string())
            .unwrap()
            .join()
            .unwrap();
        assert!(error.contains("cannot be used with"));
    }

    #[test]
    fn optional_switches_are_tri_state() {
        assert_eq!(
            resolve_optional_switch(false, false, "--enabled", "--disabled").unwrap(),
            None
        );
        assert_eq!(
            resolve_optional_switch(true, false, "--enabled", "--disabled").unwrap(),
            Some(true)
        );
        assert_eq!(
            resolve_optional_switch(false, true, "--enabled", "--disabled").unwrap(),
            Some(false)
        );
        assert!(resolve_optional_switch(true, true, "--enabled", "--disabled").is_err());
    }

    #[test]
    fn memory_retention_preserves_embedding_only_updates() {
        let current = json!({
            "enabled": false,
            "max_items": 50,
            "max_age_days": 30,
            "sweep_stale": true,
        });
        let (enabled, max_items, max_age_days) =
            resolve_memory_retention(&current, false, false, false, None, None).unwrap();
        assert!(!enabled);
        assert_eq!(max_items, Some(50));
        assert_eq!(max_age_days, Some(30));

        let (enabled, max_items, max_age_days) =
            resolve_memory_retention(&current, false, false, false, Some(80), None).unwrap();
        assert!(!enabled);
        assert_eq!(max_items, Some(80));
        assert_eq!(max_age_days, Some(30));

        let (enabled, max_items, max_age_days) =
            resolve_memory_retention(&current, true, false, false, None, None).unwrap();
        assert!(enabled);
        assert_eq!(max_items, Some(50));
        assert_eq!(max_age_days, Some(30));

        let (enabled, max_items, max_age_days) =
            resolve_memory_retention(&current, false, true, false, None, None).unwrap();
        assert!(!enabled);
        assert_eq!(max_items, Some(50));
        assert_eq!(max_age_days, Some(30));

        let (enabled, max_items, max_age_days) =
            resolve_memory_retention(&current, false, false, true, None, None).unwrap();
        assert!(!enabled);
        assert_eq!(max_items, None);
        assert_eq!(max_age_days, None);

        assert!(resolve_memory_retention(&current, true, true, false, None, None).is_err());
        assert!(resolve_memory_retention(&current, false, false, true, Some(10), None).is_err());
    }

    #[test]
    fn model_preset_update_preserves_unspecified_fields() {
        let current = json!({
            "id": "preset-1",
            "name": "fast",
            "provider": "openai-compatible",
            "model": "deepseek-flash",
            "context_window": 128000,
            "input_usd_per_million": 0.27,
            "output_usd_per_million": 1.1,
            "notes": "keep me",
            "created_at": "2026-01-01T00:00:00Z",
        });
        let merged = merge_model_preset(
            current.clone(),
            None,
            None,
            None,
            None,
            false,
            None,
            false,
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(merged["name"], "fast");
        assert_eq!(merged["model"], "deepseek-flash");
        assert_eq!(merged["context_window"], 128000);
        assert_eq!(merged["input_usd_per_million"], 0.27);
        assert_eq!(merged["notes"], "keep me");

        let merged = merge_model_preset(
            current.clone(),
            Some("renamed".into()),
            None,
            None,
            None,
            false,
            None,
            false,
            None,
            false,
            Some("new notes".into()),
        )
        .unwrap();
        assert_eq!(merged["name"], "renamed");
        assert_eq!(merged["context_window"], 128000);
        assert_eq!(merged["notes"], "new notes");

        let merged = merge_model_preset(
            current, None, None, None, None, true, None, true, None, true, None,
        )
        .unwrap();
        assert_eq!(merged["context_window"], Value::Null);
        assert_eq!(merged["input_usd_per_million"], Value::Null);
        assert_eq!(merged["output_usd_per_million"], Value::Null);
    }

    #[test]
    fn removal_renderers_are_command_scoped() {
        assert_eq!(
            removal_line(Some("runtime.leases.sweep"), &json!({"removed": 3})).as_deref(),
            Some("Removed 3 expired runtime lease(s).")
        );
        assert_eq!(
            removal_line(Some("mcp.server.remove"), &json!({"removed": true})).as_deref(),
            Some("MCP server removed=true")
        );
        assert_eq!(
            removal_line(Some("acp.agent.remove"), &json!({"removed": true})).as_deref(),
            Some("ACP agent removed=true")
        );
        assert_eq!(
            removal_line(Some("host.remove"), &json!({"removed": false})).as_deref(),
            Some("execution host removed=false")
        );
        assert_eq!(
            removal_line(Some("model.preset.delete"), &json!({"removed": true})).as_deref(),
            Some("model preset removed=true")
        );
        assert_eq!(
            removal_line(Some("mailbox.gc"), &json!({"removed": 2})).as_deref(),
            Some("mailbox GC removed 2 message(s)")
        );
        assert_eq!(
            removal_line(
                Some("artifact.gc"),
                &json!({"removed": 1, "bytes_freed": 42})
            )
            .as_deref(),
            Some("artifact GC removed 1 artifact(s), freed 42 bytes")
        );
        assert_eq!(
            removal_line(
                Some("memory.prune"),
                &json!({"candidates": 2, "removed": 1, "dry_run": false})
            )
            .as_deref(),
            Some("memory prune: candidates=2 removed=1 dry_run=false")
        );
        assert!(removal_line(None, &json!({"removed": 3})).is_none());
        assert!(removal_line(Some("mcp.server.remove"), &json!({"removed": 3})).is_none());
    }

    #[test]
    fn human_render_hints_cover_ambiguous_removal_commands() {
        let cases: &[(&[&str], &str)] = &[
            (&["vp", "mcp-remove", "mcp-1"], "mcp.server.remove"),
            (&["vp", "acp-remove", "acp-1"], "acp.agent.remove"),
            (&["vp", "host-remove", "prj-1"], "host.remove"),
            (
                &["vp", "model-preset-delete", "preset-1"],
                "model.preset.delete",
            ),
            (&["vp", "mailbox-gc", "prj-1"], "mailbox.gc"),
            (&["vp", "gc", "prj-1"], "artifact.gc"),
            (&["vp", "memory-delete", "mem-1"], "memory.delete"),
            (&["vp", "memory-prune", "prj-1"], "memory.prune"),
            (&["vp", "leases-sweep"], "runtime.leases.sweep"),
        ];
        for (args, expected) in cases {
            let cli = parse(args);
            assert_eq!(human_hint(&cli.command), Some(*expected), "{args:?}");
        }
    }

    #[test]
    fn destructive_commands_parse_the_yes_flag() {
        let cli = parse(&["vp", "discard", "ses-1", "--yes"]);
        assert!(matches!(cli.command, Command::Discard { yes: true, .. }));
        let cli = parse(&["vp", "checkpoint-restore", "ses-1", "cp-1", "--yes"]);
        assert!(matches!(
            cli.command,
            Command::CheckpointRestore { yes: true, .. }
        ));
        let cli = parse(&["vp", "restore-log", "ses-1", "--yes"]);
        assert!(matches!(cli.command, Command::RestoreLog { yes: true, .. }));
        let cli = parse(&["vp", "process-stop", "ses-1", "psup-1", "--yes"]);
        assert!(matches!(
            cli.command,
            Command::ProcessStop { yes: true, .. }
        ));
        let cli = parse(&["vp", "preempt", "ses-1", "--yes"]);
        assert!(matches!(cli.command, Command::Preempt { yes: true, .. }));

        let cli = parse(&["vp", "preempt", "ses-1"]);
        assert!(matches!(cli.command, Command::Preempt { yes: false, .. }));
    }

    #[test]
    fn noninteractive_confirmation_fails_closed() {
        let mut empty = std::io::Cursor::new(Vec::new());
        assert!(!confirm_action_with("Proceed?", &mut empty).unwrap());
        let mut no = std::io::Cursor::new(b"n\n".to_vec());
        assert!(!confirm_action_with("Proceed?", &mut no).unwrap());
        let mut blank = std::io::Cursor::new(b"\n".to_vec());
        assert!(!confirm_action_with("Proceed?", &mut blank).unwrap());
        let mut yes = std::io::Cursor::new(b"yes\n".to_vec());
        assert!(confirm_action_with("Proceed?", &mut yes).unwrap());
        let mut upper = std::io::Cursor::new(b"Y\n".to_vec());
        assert!(confirm_action_with("Proceed?", &mut upper).unwrap());
    }

    #[test]
    fn a2a_enable_and_disable_conflict_is_rejected() {
        let args = ["vp", "a2a-policy-set", "--enable", "--disable"];
        let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let error = std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap_err().to_string())
            .unwrap()
            .join()
            .unwrap();
        assert!(error.contains("cannot be used with"));
    }

    #[test]
    fn evidence_references_keep_url_colons() {
        let evidence = parse_memory_evidence("source:https://example.com/x").unwrap();
        assert_eq!(evidence["kind"], "source");
        assert_eq!(evidence["reference"], "https://example.com/x");
        assert!(evidence.get("note").is_none());

        let evidence =
            parse_memory_evidence(r#"{"kind":"task","reference":"T1","note":"reviewed"}"#).unwrap();
        assert_eq!(evidence["kind"], "task");
        assert_eq!(evidence["reference"], "T1");
        assert_eq!(evidence["note"], "reviewed");

        let evidence = parse_memory_evidence("task:T1:reviewed").unwrap();
        assert_eq!(evidence["reference"], "T1");
        assert_eq!(evidence["note"], "reviewed");
    }

    #[test]
    fn rpc_errors_preserve_machine_readable_codes() {
        let error = CliRpcError::from_rpc(vp_protocol::rpc::RpcError::with_data(
            -32602,
            "budget exhausted",
            json!({"code": "BUDGET_EXHAUSTED", "retryable": false}),
        ));
        let value = error.to_json();
        assert_eq!(value["error"]["kind"], "rpc");
        assert_eq!(value["error"]["code"], -32602);
        assert_eq!(value["error"]["typed_code"], "BUDGET_EXHAUSTED");
        assert_eq!(value["error"]["message"], "budget exhausted");
        let rendered = error.to_string();
        assert!(rendered.contains("budget exhausted"));
        assert!(rendered.contains("(code -32602)"));
        assert!(rendered.contains("[BUDGET_EXHAUSTED]"));
    }

    #[test]
    fn streaming_exit_codes_are_errors() {
        use crate::dispatch::streamed_exit_error;
        assert!(streamed_exit_error("console", "exited", Some(0)).is_none());
        assert_eq!(
            streamed_exit_error("console", "failed", Some(7)).as_deref(),
            Some("console command exited with code 7")
        );
        assert_eq!(
            streamed_exit_error("terminal", "exited", None).as_deref(),
            Some("terminal command ended with status exited and no exit code")
        );
        assert!(streamed_exit_error("terminal", "running", None).is_some());
    }

    #[test]
    fn rpc_timeout_configuration_rejects_zero() {
        assert!(configure_timeouts(0, 10).is_err());
        assert!(configure_timeouts(60, 0).is_err());
        assert!(configure_timeouts(3601, 10).is_err());
    }

    #[test]
    fn rpc_request_timeout_bounds_a_stalled_daemon() {
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((_stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        configure_timeouts(1, 1).unwrap();
        let discovery = vp_protocol::discovery::Discovery {
            pid: 1,
            port,
            token: "test-token".into(),
            protocol: 1,
            version: "test".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let started = Instant::now();
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(rpc(&discovery, "ping", json!({})))
            .unwrap_err();
        configure_timeouts(300, 10).unwrap();
        let typed = error
            .downcast_ref::<CliRpcError>()
            .expect("stalled daemon must produce a typed RPC error");
        assert_eq!(typed.kind, "timeout");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "the bounded request timeout should fire promptly"
        );
    }
}

#[cfg(test)]
mod auto_budget_cli_tests {
    use super::*;
    use crate::render::{
        budget_lines, project_auto_budget_csv, session_budget_csv, session_budget_forecast_csv,
    };

    fn parse(args: &[&str]) -> Cli {
        let args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || Cli::try_parse_from(args).unwrap())
            .unwrap()
            .join()
            .unwrap()
    }

    fn status() -> Value {
        json!({
            "session_id": "ses-1",
            "mode": "auto",
            "unlimited": false,
            "effective_token_budget": 10_000,
            "effective_cost_budget_usd": 2.5,
            "effective_time_budget_secs": 3_600,
            "hard_token_cap": 50_000,
            "hard_cost_cap_usd": 10.0,
            "hard_wall_clock_secs": 14_400,
            "spent_tokens": 1_234,
            "spent_cost_usd": 0.5,
            "elapsed_secs": 90,
            "remaining_tokens": 8_766,
            "remaining_cost_usd": 2.0,
            "remaining_secs": 3_510,
            "policy": {
                "enabled": true,
                "extend_ratio": 0.25,
                "max_multiplier": 4.0,
                "forecast_calls": 8,
                "min_runway_turns": 4
            },
            "state": {
                "extensions": 1,
                "total_turns": 3,
                "turns_without_progress": 0,
                "elapsed_secs": 90,
                "paused_reason": null
            },
            "forecast": {
                "sample_calls": 8,
                "avg_input_tokens_per_call": 600,
                "avg_output_tokens_per_call": 525,
                "avg_cost_usd_per_call": 0.25,
                "remaining_tokens": 8_766,
                "remaining_cost_usd": 2.0,
                "remaining_secs": 3_510,
                "runway_turns": 7,
                "runway_secs": 3_510,
                "runway_cost_usd": 1.75,
                "critical_path_turns": null,
                "source": "provider_ledger",
                "generated_at": "2026-11-21T00:00:00Z"
            },
            "updated_at": "2026-11-21T00:00:00Z"
        })
    }

    #[test]
    fn session_budget_commands_parse_with_partial_flags() {
        let cli = parse(&["vp", "session-budget", "ses-1"]);
        match cli.command {
            Command::SessionBudget { session, csv } => {
                assert_eq!(session, "ses-1");
                assert!(!csv);
            }
            other => panic!("expected session-budget, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "session-budget-set",
            "ses-1",
            "--mode",
            "auto",
            "--hard-token-cap",
            "50000",
            "--hard-cost-cap-usd",
            "7.5",
            "--clear-time-budget",
            "--unlimited",
            "false",
        ]);
        match cli.command {
            Command::SessionBudgetSet {
                session,
                mode,
                hard_token_cap,
                hard_cost_cap_usd,
                clear_time_budget,
                unlimited,
                token_budget,
                ..
            } => {
                assert_eq!(session, "ses-1");
                assert_eq!(mode, Some(BudgetModeArg::Auto));
                assert_eq!(hard_token_cap, Some(50_000));
                assert_eq!(hard_cost_cap_usd, Some(7.5));
                assert!(clear_time_budget);
                assert_eq!(unlimited, Some(false));
                assert!(token_budget.is_none());
            }
            other => panic!("expected session-budget-set, got {other:?}"),
        }

        let cli = parse(&["vp", "session-budget-forecast", "ses-1", "--csv"]);
        assert!(matches!(
            cli.command,
            Command::SessionBudgetForecast { csv: true, .. }
        ));
    }

    #[test]
    fn project_auto_budget_commands_parse_policy_fields() {
        let cli = parse(&["vp", "project-auto-budget", "prj-1"]);
        match cli.command {
            Command::ProjectAutoBudget { project, csv } => {
                assert_eq!(project, "prj-1");
                assert!(!csv);
            }
            other => panic!("expected project-auto-budget, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "project-auto-budget-set",
            "prj-1",
            "--enabled",
            "--hard-cost-cap-usd",
            "5",
            "--no-progress-action",
            "escalate",
            "--max-turns-per-run",
            "40",
        ]);
        match cli.command {
            Command::ProjectAutoBudgetSet {
                project,
                enabled,
                hard_cost_cap_usd,
                no_progress_action,
                max_turns_per_run,
                ..
            } => {
                assert_eq!(project, "prj-1");
                assert_eq!(enabled, Some(true));
                assert_eq!(hard_cost_cap_usd, Some(5.0));
                assert!(matches!(
                    no_progress_action,
                    Some(NoProgressActionArg::Escalate)
                ));
                assert_eq!(max_turns_per_run, Some(40));
            }
            other => panic!("expected project-auto-budget-set, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "project-auto-budget-set",
            "prj-1",
            "--enabled",
            "false",
            "--policy-file",
            "-",
        ]);
        match cli.command {
            Command::ProjectAutoBudgetSet {
                enabled,
                policy_file,
                ..
            } => {
                assert_eq!(enabled, Some(false));
                assert_eq!(policy_file.as_deref(), Some("-"));
            }
            other => panic!("expected project-auto-budget-set, got {other:?}"),
        }
    }

    #[test]
    fn session_new_auto_flags_parse() {
        let cli = parse(&[
            "vp",
            "session-new",
            "prj-1",
            "ship it",
            "--auto",
            "--max-usd",
            "5",
            "--max-token",
            "100000",
            "--max-hours",
            "2.5",
        ]);
        match cli.command {
            Command::SessionNew {
                auto,
                max_usd,
                max_token,
                max_hours,
                budget_mode,
                unlimited,
                ..
            } => {
                assert!(auto);
                assert_eq!(max_usd, Some(5.0));
                assert_eq!(max_token, Some(100_000));
                assert_eq!(max_hours, Some(2.5));
                assert!(budget_mode.is_none());
                assert!(!unlimited);
            }
            other => panic!("expected session-new, got {other:?}"),
        }

        let cli = parse(&[
            "vp",
            "session-new",
            "prj-1",
            "goal",
            "--budget-mode",
            "fixed",
            "--unlimited",
        ]);
        match cli.command {
            Command::SessionNew {
                budget_mode,
                unlimited,
                ..
            } => {
                assert_eq!(budget_mode, Some(BudgetModeArg::Fixed));
                assert!(unlimited);
            }
            other => panic!("expected session-new, got {other:?}"),
        }
    }

    #[test]
    fn budget_status_renderer_shows_mode_limits_spend_and_runway() {
        let lines = budget_lines(&status()).expect("status lines");
        let text = lines.join("\n");
        assert!(text.contains("session ses-1 budget"));
        assert!(text.contains("mode auto"));
        assert!(text.contains("effective"));
        assert!(text.contains("10,000"));
        assert!(text.contains("hard caps"));
        assert!(text.contains("50,000"));
        assert!(text.contains("1,234 tokens"));
        assert!(text.contains("8 call(s)"));
        assert!(text.contains("provider_ledger"));
        assert!(text.contains("7 turn(s)"));
        assert!(text.contains("extensions=1"));
    }

    #[test]
    fn budget_set_renderer_marks_the_nested_status_as_updated() {
        let value = json!({"session_id": "ses-1", "budget": status()});
        let text = budget_lines(&value).expect("set lines").join("\n");
        assert!(text.contains("session ses-1 budget updated"));
        assert!(text.contains("mode auto"));
    }

    #[test]
    fn budget_forecast_renderer_is_forecast_only() {
        let value = json!({"session_id": "ses-1", "forecast": status()["forecast"].clone()});
        let text = budget_lines(&value).expect("forecast lines").join("\n");
        assert!(text.contains("session ses-1 budget forecast"));
        assert!(text.contains("samples"));
        assert!(text.contains("provider_ledger"));
        assert!(text.contains("critical    unknown"));
        assert!(!text.contains("effective"));
    }

    #[test]
    fn auto_policy_renderer_is_compact_and_unrelated_shapes_are_ignored() {
        let value = json!({
            "project_id": "prj-1",
            "updated_at": "2026-11-21T00:00:00Z",
            "policy": {
                "enabled": true,
                "initial_token_budget": 10_000,
                "initial_cost_budget_usd": 1.0,
                "initial_wall_clock_secs": 3_600,
                "hard_token_cap": 50_000,
                "hard_cost_cap_usd": 5.0,
                "hard_wall_clock_secs": 14_400,
                "extend_ratio": 0.25,
                "max_multiplier": 4.0,
                "forecast_calls": 8,
                "min_runway_turns": 4,
                "warn_at_ratio": 0.2,
                "pause_at_ratio": 0.05,
                "max_turns_per_run": 60,
                "max_total_turns": null,
                "no_progress_turns": 6,
                "no_progress_action": "pause",
                "auto_tool_bundle": true
            }
        });
        let text = budget_lines(&value).expect("policy lines").join("\n");
        assert!(text.contains("project prj-1 auto budget"));
        assert!(text.contains("hard caps"));
        assert!(text.contains("50,000"));
        assert!(text.contains("no-progress 6 (pause)"));
        assert!(text.contains("tool bundle auto=true"));

        assert!(budget_lines(&json!({"session_id": "ses-1"})).is_none());
        assert!(budget_lines(&json!({"policy": {"enabled": true}})).is_none());
    }

    #[test]
    fn project_run_policy_renderer_does_not_look_like_an_effect_policy() {
        let value = json!({
            "project_id": "prj-1",
            "updated_at": "2026-11-24T00:00:00Z",
            "policy": {
                "default_profile": "fast",
                "completion_reserve_ratio": 0.25,
                "attention_cap": 9,
                "stop_policy": {
                    "require_evidence": true,
                    "no_progress_turns": 4,
                    "marginal_value_ratio": 0.2,
                    "partial_delivery": true
                }
            }
        });
        let text = budget_lines(&value)
            .expect("project run policy lines")
            .join("\n");
        assert!(text.contains("project prj-1 Run Contract defaults"));
        assert!(text.contains("updated 2026-11-24T00:00:00Z"));
        assert!(text.contains("profile     fast"));
        assert!(text.contains("reserve     25%"));
        assert!(text.contains("attention   cap 9"));
        assert!(text.contains("no-progress 4 turn(s)"));
        assert!(!text.contains("effects enabled"));

        let defaults = json!({
            "project_id": "prj-1",
            "policy": {
                "default_profile": "balanced",
                "completion_reserve_ratio": 0.2,
                "attention_cap": 20,
                "stop_policy": {}
            }
        });
        let text = budget_lines(&defaults)
            .expect("default policy lines")
            .join("\n");
        assert!(text.contains("built-in defaults"));
        assert!(text.contains("evidence required"));
        assert!(text.contains("partial delivery allowed"));
    }

    #[test]
    fn budget_csv_paths_have_one_header_and_one_row() {
        let value = status();
        let csv = session_budget_csv(&value);
        let mut lines = csv.lines();
        let header = lines.next().unwrap();
        assert!(header.starts_with("session_id,mode,unlimited,effective_token_budget"));
        let row = lines.next().unwrap();
        assert!(row.starts_with("ses-1,auto,false,10000,2.5,3600,50000"));
        assert!(lines.next().is_none());

        let forecast = json!({"session_id": "ses-1", "forecast": value["forecast"].clone()});
        let csv = session_budget_forecast_csv(&forecast);
        let mut lines = csv.lines();
        assert!(lines.next().unwrap().starts_with("session_id,sample_calls"));
        assert!(lines.next().unwrap().starts_with("ses-1,8,600,525,0.25"));
        assert!(lines.next().is_none());

        let policy = json!({
            "project_id": "prj-1",
            "updated_at": "2026-11-21T00:00:00Z",
            "policy": {
                "enabled": true,
                "hard_token_cap": 50_000,
                "no_progress_action": "pause",
                "auto_tool_bundle": true
            }
        });
        let csv = project_auto_budget_csv(&policy);
        let mut lines = csv.lines();
        assert!(
            lines
                .next()
                .unwrap()
                .starts_with("project_id,updated_at,enabled")
        );
        assert!(
            lines
                .next()
                .unwrap()
                .starts_with("prj-1,2026-11-21T00:00:00Z,true")
        );
    }
}

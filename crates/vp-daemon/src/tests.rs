use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vp_core::Engine;
use vp_core::agent::AgentService;
use vp_protocol::diagnosis::{
    SessionAttributionsParams, SessionDiagnosisParams, SessionDiagnosisRecordParams,
};
use vp_protocol::now_rfc3339;
use vp_protocol::otel::{OtelExportParams, OtelStatusParams, OtelTestParams, SetOtelPolicyParams};
use vp_protocol::replay::ControllerReplayParams;
use vp_protocol::rpc::{RpcError, codes};
use vp_protocol::runtime::{PlanMigrateParams, SessionCompatibilityParams};
use vp_store::Store;

use crate::AppState;
use crate::backup_status::backup_status_in;
use crate::dispatch::dispatch;

#[cfg(test)]
mod project_preflight_tests;
use crate::errors::{
    coordination_error, diagnosis_error, identity_error, otel_error, runtime_pin_error,
    workspace_error,
};
use crate::params::parse_params;

#[cfg(test)]
mod process_rpc_tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;
    use vp_core::process_supervisor::{ProcessSupervisor, StartSpec};
    use vp_core::tools::ToolContext;
    use vp_protocol::rpc::RpcRequest;

    fn context(
        worktree: &Path,
        data: &Path,
        _store: &Store,
        session: &str,
        supervisor: Arc<ProcessSupervisor>,
    ) -> ToolContext {
        ToolContext {
            root: worktree.to_path_buf(),
            project_root: worktree.to_path_buf(),
            sandbox_root: data.join("sandbox").join(session),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: worktree.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: Some(session.to_string()),
            agent_id: None,
            task_id: None,
            process_supervisor: Some(supervisor),
            test_cache: None,
        }
    }

    async fn call(state: &AppState, method: &str, params: Value) -> Value {
        let response = dispatch(state, RpcRequest::new(json!(1), method, params)).await;
        assert!(
            response.error.is_none(),
            "{} failed: {:?}",
            method,
            response.error
        );
        response.result.unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn process_rpcs_and_metrics_cover_the_durable_surface() {
        let worktree = TempDir::new().unwrap();
        std::fs::create_dir_all(worktree.path().join("home")).unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store
            .create_session(&project.id, "process rpc test")
            .unwrap();
        let engine = Engine::new(store.clone());
        let supervisor = Arc::clone(engine.process_supervisor());
        let context = context(
            worktree.path(),
            data.path(),
            &store,
            &session.id,
            supervisor.clone(),
        );
        let row = supervisor
            .start(
                &session.id,
                &context,
                &StartSpec {
                    command: "printf 'rpc-ready\\n'; sleep 20".to_string(),
                    ..StartSpec::default()
                },
            )
            .await
            .unwrap();
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let list = call(
            &state,
            "session.processes",
            json!({"session_id": session.id, "include_finished": false, "limit": 10}),
        )
        .await;
        assert_eq!(list["session_id"], session.id);
        assert_eq!(list["active"], 1);
        assert_eq!(list["processes"][0]["id"], row.id);

        let logs = call(
            &state,
            "session.process.logs",
            json!({
                "session_id": session.id,
                "process_id": row.id,
                "from_end": true,
                "max_bytes": 1024,
            }),
        )
        .await;
        assert_eq!(logs["process_id"], row.id);
        assert_eq!(logs["state"], "running");
        let content = logs["content"].as_str().unwrap_or_default();
        // The shell may not have flushed the first line yet on a loaded CI box;
        // poll briefly rather than asserting on a scheduling race.
        if !content.contains("rpc-ready") {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let logs = call(
                    &state,
                    "session.process.logs",
                    json!({
                        "session_id": session.id,
                        "process_id": row.id,
                        "from_end": true,
                        "max_bytes": 1024,
                    }),
                )
                .await;
                if logs["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("rpc-ready")
                {
                    break;
                }
            }
        }

        let metrics = call(&state, "daemon.metrics", json!({})).await;
        let process_counts = metrics["metrics"]["supervised_processes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            process_counts
                .iter()
                .any(|entry| entry["key"] == "running" && entry["count"].as_u64() == Some(1)),
            "metrics={metrics}"
        );
        assert!(
            metrics["metrics"]["supervised_process_output_bytes"]
                .as_u64()
                .is_some()
        );
        assert!(
            metrics["prometheus"]
                .as_str()
                .unwrap_or_default()
                .contains("verlinpark_supervised_processes{state=\"running\"} 1")
        );

        let policy = call(
            &state,
            "process.policy.get",
            json!({"project_id": project.id}),
        )
        .await;
        assert_eq!(policy["policy"]["max_processes"], 8);
        let updated = call(
            &state,
            "process.policy.set",
            json!({
                "project_id": project.id,
                "policy": {
                    "idle_sweep_enabled": false,
                    "max_processes": 2,
                    "idle_ttl_secs": 600,
                    "max_output_bytes": 65536,
                    "max_output_lines": 2000
                }
            }),
        )
        .await;
        assert_eq!(updated["policy"]["max_processes"], 2);
        assert_eq!(updated["policy"]["idle_sweep_enabled"], false);
        let reread = call(
            &state,
            "process.policy.get",
            json!({"project_id": project.id}),
        )
        .await;
        assert_eq!(reread["policy"]["idle_ttl_secs"], 600);

        let stopped = call(
            &state,
            "session.process.stop",
            json!({
                "session_id": session.id,
                "process_id": row.id,
                "signal": "TERM",
                "grace_secs": 2,
            }),
        )
        .await;
        assert_eq!(stopped["process"]["state"], "stopped");
        assert_eq!(stopped["forced"], false);

        let missing = dispatch(
            &state,
            RpcRequest::new(
                json!(2),
                "session.process.logs",
                json!({"session_id": session.id, "process_id": "psup_missing"}),
            ),
        )
        .await;
        assert!(missing.error.is_some());
    }

    #[tokio::test]
    async fn backup_status_reports_anchor_without_secret_material() {
        let data = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let repository_id = "0190f000-0000-7000-8000-000000000000";
        let anchor = vp_core::backup::anchor::AnchorStore::new(data.path());
        anchor
            .record_verified(repository_id, "/backups/repo", None, 4, "verify", false)
            .unwrap();
        let status = backup_status_in(data.path(), keys.path()).unwrap();
        assert!(status.anchor_present);
        assert_eq!(status.repositories.len(), 1);
        assert_eq!(status.repositories[0].highest_generation, 4);
        assert_eq!(status.repositories[0].last_verified_generation, 4);
        assert_eq!(status.signed_repository_formats, vec![3]);
        assert_eq!(status.revoked_key_count, 0);
        assert_eq!(
            status.portable_anchor_format_version,
            vp_core::backup::anchor::PORTABLE_ANCHOR_FORMAT_VERSION
        );
        assert_eq!(status.imported_repositories, 0);
        assert!(status.note.contains("not tamper-proof"));
        assert!(status.note.contains("allow-revoked"));
        let rendered = serde_json::to_string(&status).unwrap();
        assert!(!rendered.contains("seed_b64"));
        assert!(!rendered.contains("private_key"));
        assert!(!rendered.contains("passphrase"));
    }

    #[tokio::test]
    async fn backup_status_reports_revoked_keys_and_portable_imports_without_secrets() {
        use vp_core::backup::anchor::{AnchorStore, PortableAnchorImportOptions};
        use vp_core::backup::signing::{Keyring, SecretSource};

        let data = TempDir::new().unwrap();
        let keys = TempDir::new().unwrap();
        let source_data = TempDir::new().unwrap();
        Keyring::generate_key(
            keys.path(),
            "old",
            true,
            SecretSource::File {
                name: "old.key".to_string(),
            },
        )
        .unwrap();
        Keyring::generate_key(
            keys.path(),
            "new",
            true,
            SecretSource::File {
                name: "new.key".to_string(),
            },
        )
        .unwrap();
        let mut keyring = Keyring::load(keys.path()).unwrap();
        keyring.revoke_key("old", false).unwrap();

        let repository_id = "0190f000-0000-7000-8000-000000000011";
        let source = AnchorStore::new(source_data.path());
        source
            .record_verified(repository_id, "/source/repo", None, 7, "verify", false)
            .unwrap();
        let export = source
            .export_portable(&Keyring::load(keys.path()).unwrap(), None, None)
            .unwrap();
        let target = AnchorStore::new(data.path());
        target
            .import_portable(
                &export.document,
                PortableAnchorImportOptions {
                    keyring: &Keyring::load(keys.path()).unwrap(),
                    allow_revoked: false,
                    reject_stale: false,
                },
            )
            .unwrap();

        let status = backup_status_in(data.path(), keys.path()).unwrap();
        assert_eq!(status.active_key_id.as_deref(), Some("new"));
        assert_eq!(status.revoked_key_count, 1);
        assert_eq!(status.imported_repositories, 1);
        assert_eq!(status.portable_anchor_format_version, 1);
        let old = status.keys.iter().find(|key| key.key_id == "old").unwrap();
        assert!(old.revoked_at.is_some());
        assert!(!old.active);
        assert_eq!(
            status.repositories[0].origin.as_deref(),
            Some("portable_import")
        );
        assert_eq!(status.repositories[0].last_operation, "anchor_import");

        let rendered = serde_json::to_string(&status).unwrap();
        assert!(!rendered.contains("seed_b64"));
        assert!(!rendered.contains("private_key"));
        assert!(!rendered.contains("passphrase"));
    }

    #[tokio::test]
    async fn preemption_rpcs_round_trip_policy_and_report_no_active_run() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        std::fs::write(repo.path().join("README.md"), "preempt\n").unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("preempt"))
            .unwrap();
        let session = store.create_session(&project.id, "preemption rpc").unwrap();
        let engine = Engine::new(store.clone());
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };
        let current = call(
            &state,
            "project.preemption_policy.get",
            json!({"project_id": project.id}),
        )
        .await;
        assert_eq!(current["policy"]["enabled"], true);
        assert_eq!(current["policy"]["provider_grace_ms"], 2_000);
        let updated = call(
            &state,
            "project.preemption_policy.set",
            json!({
                "project_id": project.id,
                "policy": {
                    "enabled": true,
                    "provider_grace_ms": 999_999,
                    "tool_grace_ms": 1_500,
                    "kill_escalation_ms": 500,
                    "tool_deadline_secs": 9_999,
                    "checkpoint_on_preempt": false
                }
            }),
        )
        .await;
        assert_eq!(updated["policy"]["provider_grace_ms"], 30_000);
        assert_eq!(updated["policy"]["tool_grace_ms"], 1_500);
        assert_eq!(updated["policy"]["tool_deadline_secs"], 3_600);
        assert_eq!(updated["policy"]["checkpoint_on_preempt"], false);

        let report = call(
            &state,
            "session.preemption",
            json!({"session_id": session.id, "limit": 10}),
        )
        .await;
        assert_eq!(report["session_id"], session.id);
        assert_eq!(report["preempted"], false);
        assert_eq!(report["active"], false);
        assert_eq!(report["policy"]["tool_grace_ms"], 1_500);
        assert!(report["records"].as_array().unwrap().is_empty());

        let preempt = call(
            &state,
            "session.preempt",
            json!({"session_id": session.id, "reason": "rpc test"}),
        )
        .await;
        assert_eq!(preempt["preempted"], false);
        assert!(
            preempt["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("no active run")
        );

        engine
            .append_event(
                &session.id,
                "session.preempted",
                json!({
                    "session_id": session.id,
                    "reason": "wall_clock",
                    "phase": "provider",
                    "elapsed_ms": 1005,
                    "terminal": true,
                    "created_at": now_rfc3339()
                }),
            )
            .await
            .unwrap();
        let metrics = call(&state, "daemon.metrics", json!({})).await;
        let phases = metrics["metrics"]["preemptions_by_phase"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            phases
                .iter()
                .any(|entry| entry["key"] == "provider" && entry["count"].as_u64() == Some(1)),
            "metrics={metrics}"
        );
        assert!(
            metrics["prometheus"]
                .as_str()
                .unwrap_or_default()
                .contains("verlinpark_preemptions{phase=\"provider\"} 1")
        );
        let overview = call(
            &state,
            "preemption.overview",
            json!({"project_id": project.id, "limit": 10}),
        )
        .await;
        assert_eq!(overview["total"], 1);
        assert_eq!(overview["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(overview["sessions"][0]["last_reason"], "wall_clock");
        assert_eq!(overview["sessions"][0]["last_phase"], "provider");
        assert_eq!(overview["by_phase"][0]["key"], "provider");
    }

    #[tokio::test]
    async fn forge_status_and_preflight_rpcs_are_secret_free() {
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let engine = Engine::new(store);
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let status = call(&state, "forge.status", json!({})).await;
        assert_eq!(status["default_provider"], "github");
        let providers = status["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 4);
        let bitbucket = providers
            .iter()
            .find(|provider| provider["provider"] == "bitbucket")
            .unwrap();
        assert_eq!(bitbucket["transport"], "rest");
        assert_eq!(bitbucket["change_name"], "PR");
        assert!(bitbucket.get("command").is_none());
        let serialized = status.to_string();
        assert!(!serialized.contains("BITBUCKET_APP_PASSWORD="));

        let response = dispatch(
            &state,
            RpcRequest::new(
                json!(2),
                "forge.preflight",
                json!({"project_id": "prj-missing"}),
            ),
        )
        .await;
        let error = response.error.unwrap();
        assert!(
            error.message.contains("project not found"),
            "{}",
            error.message
        );
    }
}

#[cfg(test)]
mod archive_media_rpc_tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;
    use tempfile::TempDir;
    use vp_protocol::rpc::RpcRequest;
    use vp_protocol::types::MediaTranscodeParams;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, data) in entries {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    async fn call(state: &AppState, method: &str, params: Value) -> Value {
        let response = dispatch(state, RpcRequest::new(json!(1), method, params)).await;
        assert!(
            response.error.is_none(),
            "{} failed: {:?}",
            method,
            response.error
        );
        response.result.unwrap_or(Value::Null)
    }

    async fn call_error(state: &AppState, method: &str, params: Value) -> RpcError {
        let response = dispatch(state, RpcRequest::new(json!(1), method, params)).await;
        response
            .error
            .unwrap_or_else(|| panic!("{method} unexpectedly succeeded"))
    }

    #[tokio::test]
    async fn archive_and_media_rpcs_return_typed_results_and_failures() {
        let worktree = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), Some("archive-rpc"))
            .unwrap();
        let session = store.create_session(&project.id, "archive rpc").unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "bundle.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("a.txt", b"alpha"), ("dir/b.txt", b"beta")]),
            )
            .await
            .unwrap();
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let listing = call(
            &state,
            "artifact.archive.list",
            json!({"artifact_id": artifact.id}),
        )
        .await;
        assert_eq!(listing["format"], "zip");
        assert_eq!(listing["entry_count"], 2);
        assert_eq!(listing["entries"][1]["path"], "dir/b.txt");
        assert_eq!(listing["entries"][1]["safe"], true);

        let extraction = call(
            &state,
            "artifact.archive.extract",
            json!({
                "artifact_id": artifact.id,
                "entries": ["dir/b.txt"],
                "include_previews": true
            }),
        )
        .await;
        assert_eq!(extraction["entries"][0]["preview"], "beta");
        assert_eq!(extraction["scratch_retained"], false);
        let derived_id = extraction["published_artifacts"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let derived = call(&state, "artifact.get", json!({"artifact_id": derived_id})).await;
        assert_eq!(derived["provenance"]["operation"], "archive_extract");
        assert_eq!(derived["provenance"]["source_artifact_id"], artifact.id);
        assert_eq!(derived["provenance"]["entry_path"], "dir/b.txt");

        let malicious = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "slip.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("../escape.txt", b"bad")]),
            )
            .await
            .unwrap();
        let error = call_error(
            &state,
            "artifact.archive.extract",
            json!({
                "artifact_id": malicious.id,
                "entries": ["../escape.txt"]
            }),
        )
        .await;
        let failure = error.data.expect("typed archive failure data");
        assert_eq!(failure["code"], "ARCHIVE_UNSAFE_ENTRY");
        assert_eq!(failure["retryable"], false);

        let transcode_error = call_error(
            &state,
            "media.transcode",
            json!({
                "artifact_id": artifact.id,
                "format": "mp4",
                "approved": false
            }),
        )
        .await;
        let failure = transcode_error.data.expect("typed media failure data");
        assert_eq!(failure["code"], "APPROVAL_REQUIRED");

        let media_error =
            call_error(&state, "media.info", json!({"artifact_id": artifact.id})).await;
        let failure = media_error.data.expect("typed media failure data");
        assert!(
            matches!(
                failure["code"].as_str(),
                Some("MEDIA_TOOL_UNAVAILABLE")
                    | Some("MEDIA_PROBE_FAILED")
                    | Some("MEDIA_LIMIT_EXCEEDED")
            ),
            "{failure}"
        );
    }

    #[tokio::test]
    async fn archive_rpc_rejects_unapproved_and_unsupported_requests() {
        let worktree = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(worktree.path().to_str().unwrap(), None)
            .unwrap();
        let session = store.create_session(&project.id, "archive rpc 2").unwrap();
        let engine = Engine::new(store);
        let artifact = engine
            .publish_artifact_binary(
                &session.id,
                "tester",
                "Tester",
                None,
                "bundle.zip",
                "archive",
                "application/zip",
                &zip_bytes(&[("a.txt", b"a")]),
            )
            .await
            .unwrap();
        let state = AppState {
            engine: engine.clone(),
            agent: AgentService::from_env(engine.clone()),
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        };

        let error = call_error(
            &state,
            "artifact.archive.extract",
            json!({"artifact_id": artifact.id, "entries": []}),
        )
        .await;
        assert_eq!(error.data.unwrap()["code"], "ARCHIVE_EMPTY_SELECTION");

        let error = call_error(
            &state,
            "artifact.archive.extract",
            json!({"artifact_id": artifact.id, "entries": ["missing.txt"]}),
        )
        .await;
        assert_eq!(error.data.unwrap()["code"], "ARCHIVE_ENTRY_NOT_FOUND");

        let error = call_error(
            &state,
            "media.transcode",
            json!({
                "artifact_id": artifact.id,
                "format": "avi",
                "approved": true
            }),
        )
        .await;
        assert_eq!(error.data.unwrap()["code"], "MEDIA_FORMAT_UNSUPPORTED");
        let _ = MediaTranscodeParams {
            artifact_id: artifact.id,
            format: "mp4".to_string(),
            max_duration_secs: None,
            max_bytes: None,
            approved: false,
        };
    }
}

#[cfg(test)]
mod runtime_pin_rpc_tests {
    use super::*;

    #[test]
    fn compatibility_and_replay_params_decode_with_defaults() {
        let compatibility = parse_params::<SessionCompatibilityParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert_eq!(compatibility.session_id, "ses-1");
        assert!(compatibility.run_id.is_none());

        let migrate = parse_params::<PlanMigrateParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(!migrate.approved);
        assert!(migrate.expected_revision.is_none());

        let replay = parse_params::<ControllerReplayParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(replay.limit.is_none());
    }

    #[test]
    fn otel_params_decode_with_defaults_and_errors_are_bounded() {
        let status = parse_params::<OtelStatusParams>(json!({"project_id": "proj-1"})).unwrap();
        assert!(status.session_id.is_none());
        let export = parse_params::<OtelExportParams>(json!({"session_id": "ses-1"})).unwrap();
        assert!(export.limit.is_none());
        let test = parse_params::<OtelTestParams>(json!({"project_id": "proj-1"})).unwrap();
        assert_eq!(test.project_id, "proj-1");
        let set = parse_params::<SetOtelPolicyParams>(json!({
            "project_id": "proj-1",
            "policy": {"enabled": true}
        }))
        .unwrap();
        assert!(set.policy.enabled);
        assert_eq!(set.policy.batch_limit, 200);

        let error = vp_core::otel::otel_error(
            vp_protocol::otel::otel_codes::OTEL_ENDPOINT_NOT_LOOPBACK,
            "the endpoint is public",
        );
        let response = otel_error(Some(json!("id")), error);
        let error = response.error.expect("error");
        assert!(error.message.contains("OTEL_ENDPOINT_NOT_LOOPBACK"));
        let data = error.data.expect("data");
        assert_eq!(data["code"], "OTEL_ENDPOINT_NOT_LOOPBACK");
        assert_eq!(data["retryable"], false);
        assert!(!data.to_string().contains("api_key"));
        assert!(!data.to_string().contains("prompt"));
    }

    #[test]
    fn diagnosis_params_decode_and_typed_errors_expose_no_content() {
        let params = parse_params::<SessionDiagnosisParams>(json!({
            "session_id": "ses-1",
            "target_task_id": "t2",
            "limit": 500,
            "include_graph": true
        }))
        .unwrap();
        assert_eq!(params.session_id, "ses-1");
        assert_eq!(params.target_task_id.as_deref(), Some("t2"));
        assert_eq!(params.limit, Some(500));
        assert!(params.include_graph);
        let record = parse_params::<SessionDiagnosisRecordParams>(json!({
            "session_id": "ses-1",
            "target_task_id": "t2"
        }))
        .unwrap();
        assert_eq!(record.target_task_id.as_deref(), Some("t2"));
        let history = parse_params::<SessionAttributionsParams>(json!({
            "session_id": "ses-1"
        }))
        .unwrap();
        assert!(history.limit.is_none());

        let error = vp_core::diagnosis::DiagnosisError::new(
            vp_core::diagnosis::codes::DIAGNOSIS_SESSION_NOT_FOUND,
            "session not found: ses-missing",
        );
        let response = diagnosis_error(Some(json!("id")), error.into());
        let error = response.error.expect("error");
        let data = error.data.expect("data");
        assert_eq!(data["code"], "DIAGNOSIS_SESSION_NOT_FOUND");
        assert_eq!(data["advisory"], true);
        assert_eq!(data["retryable"], false);
        assert!(!data.to_string().contains("prompt"));
        assert!(!data.to_string().contains("output"));
    }

    #[test]
    fn runtime_pin_errors_expose_q3_data_without_secrets() {
        let report = vp_protocol::runtime::CompatibilityReport {
            status: vp_protocol::runtime::CompatibilityStatus::Incompatible,
            target: "plan".to_string(),
            session_id: "ses-1".to_string(),
            plan_revision: 2,
            warnings: vec!["contract_changed".to_string()],
            migration_required: true,
            ..Default::default()
        };
        let error = anyhow::Error::new(
            vp_core::runtime_pin::RuntimePinError::new(
                vp_core::runtime_pin::codes::RUNTIME_INCOMPATIBLE,
                "plan changed",
            )
            .with_report(report),
        );
        let response = runtime_pin_error(Some(json!("id")), error);
        let error = response.error.expect("error");
        assert!(error.message.contains("RUNTIME_INCOMPATIBLE"));
        let data = error.data.expect("data");
        assert_eq!(data["code"], "RUNTIME_INCOMPATIBLE");
        assert_eq!(data["retryable"], false);
        assert_eq!(data["compatibility"]["status"], "incompatible");
        assert!(!data.to_string().contains("api_key"));
    }
}

#[cfg(test)]
mod agent_identity_rpc_error_tests {
    use super::*;
    use vp_protocol::types::{AgentIdentityError, agent_identity_codes};

    #[test]
    fn typed_identity_errors_map_to_stable_rpc_codes_without_text_classification() {
        let response = identity_error(
            Some(json!("1")),
            anyhow::Error::new(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_NOT_FOUND,
                "agent identity not found: identity-1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("data")["code"],
            agent_identity_codes::IDENTITY_NOT_FOUND
        );

        let response = identity_error(
            Some(json!("2")),
            anyhow::Error::new(AgentIdentityError::new(
                agent_identity_codes::IDENTITY_CAP_EXCEEDED,
                "max_workers is 1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("data");
        assert_eq!(data["code"], agent_identity_codes::IDENTITY_CAP_EXCEEDED);
        assert!(!data.to_string().contains("prompt"));
        assert!(!data.to_string().contains("api_key"));
    }

    #[test]
    fn untyped_identity_failures_remain_internal_errors() {
        let response = identity_error(Some(json!("3")), anyhow::anyhow!("database is closed"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }

    #[test]
    fn s6_employment_errors_are_conflicts_and_do_not_leak_evidence_text() {
        for code in [
            agent_identity_codes::IDENTITY_BUSY,
            agent_identity_codes::HIRE_ALREADY_DECIDED,
            agent_identity_codes::RETIREMENT_NOT_QUIESCENT,
            agent_identity_codes::RETIREMENT_COOLDOWN,
        ] {
            let response = identity_error(
                Some(json!("4")),
                anyhow::Error::new(AgentIdentityError::new(code, "identity needs a decision")),
            );
            let error = response.error.expect("error");
            assert_eq!(error.code, codes::CONFLICT);
            assert_eq!(error.data.expect("data")["code"], code);
        }
        for code in [
            agent_identity_codes::RETIREMENT_APPROVAL_REQUIRED,
            agent_identity_codes::IDENTITY_APPROVAL_REQUIRED,
            agent_identity_codes::HIRE_INVALID,
        ] {
            let response = identity_error(
                Some(json!("5")),
                anyhow::Error::new(AgentIdentityError::new(code, "approval required")),
            );
            let error = response.error.expect("error");
            assert_eq!(error.code, codes::INVALID_PARAMS);
            assert_eq!(error.data.expect("data")["code"], code);
        }
    }
}

#[cfg(test)]
mod agent_coordination_rpc_error_tests {
    use super::*;
    use vp_protocol::types::{AgentCoordinationError, agent_coordination_codes};

    #[test]
    fn typed_coordination_errors_map_to_bounded_rpc_codes() {
        let response = coordination_error(
            Some(json!("1")),
            anyhow::Error::new(AgentCoordinationError::new(
                agent_coordination_codes::ASSIGNMENT_NOT_FOUND,
                "session not found: ses-1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("data")["code"],
            agent_coordination_codes::ASSIGNMENT_NOT_FOUND
        );

        let response = coordination_error(
            Some(json!("2")),
            anyhow::Error::new(AgentCoordinationError::new(
                agent_coordination_codes::ASSIGNMENT_CONFLICT,
                "identity already assigned",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("data");
        assert_eq!(data["code"], agent_coordination_codes::ASSIGNMENT_CONFLICT);
        assert!(!data.to_string().contains("prompt"));
        assert!(!data.to_string().contains("api_key"));

        let response = coordination_error(
            Some(json!("3")),
            anyhow::Error::new(vp_protocol::types::AgentIdentityError::new(
                vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND,
                "agent identity not found: id-1",
            )),
        );
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("data")["code"],
            vp_protocol::types::agent_identity_codes::IDENTITY_NOT_FOUND
        );
    }

    #[test]
    fn untyped_coordination_failures_remain_internal_errors() {
        let response = coordination_error(Some(json!("4")), anyhow::anyhow!("database closed"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }

    #[test]
    fn workspace_busy_is_a_typed_conflict_and_imported_is_read_only() {
        let busy = workspace_error(
            Some(json!("9")),
            anyhow::Error::new(vp_protocol::types::AgentWorkspaceError::new(
                vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY,
                "workspace is leased by session ses-1",
            )),
        );
        let error = busy.error.expect("error");
        assert_eq!(error.code, codes::CONFLICT);
        assert_eq!(
            error.data.expect("data")["code"],
            vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY
        );

        let imported = workspace_error(
            Some(json!("10")),
            anyhow::Error::new(vp_protocol::types::AgentWorkspaceError::new(
                vp_protocol::types::agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
                "an imported workspace is read-only evidence",
            )),
        );
        let error = imported.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        assert_eq!(
            error.data.expect("data")["code"],
            vp_protocol::types::agent_workspace_codes::WORKSPACE_IMPORTED_READONLY
        );
    }

    #[test]
    fn workspace_untyped_failures_remain_internal_errors() {
        let response = workspace_error(Some(json!("11")), anyhow::anyhow!("git unavailable"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }
}

#[cfg(test)]
mod protocol_transport_tests {
    use super::*;
    use crate::errors::{operation_error, operation_receipt_error, validation_error};
    use crate::sweepers::sweep_interval_secs;
    use axum::body::Body;
    use axum::body::to_bytes;
    use axum::extract::State;
    use axum::http::{Request as HttpRequest, StatusCode, header};
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_core::agent::AgentService;
    use vp_protocol::rpc::RpcRequest;
    use vp_protocol::types::GuiPolicy;
    use vp_store::store::Claim;

    fn init_repo(path: &Path) {
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
            if args[0] == "init" {
                std::fs::write(path.join("README.md"), "protocol daemon test\n").unwrap();
            }
        }
    }

    fn state(data: &TempDir, engine: Engine, agent: AgentService) -> AppState {
        AppState {
            engine,
            agent,
            token: Arc::new("test-token".to_string()),
            data_dir: data.path().to_path_buf(),
            started_at: now_rfc3339(),
            started: Instant::now(),
            pid: std::process::id(),
        }
    }

    pub(super) fn empty_state() -> (TempDir, AppState) {
        let data = TempDir::new().unwrap();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let agent = AgentService::unconfigured(engine.clone());
        let state = state(&data, engine, agent);
        (data, state)
    }

    async fn project_state() -> (TempDir, TempDir, AppState) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        engine
            .add_project(repo.path().to_str().unwrap().to_string(), None)
            .await
            .unwrap();
        let agent = AgentService::unconfigured(engine.clone());
        let state = state(&data, engine, agent);
        (data, repo, state)
    }

    async fn first_project_id(state: &AppState) -> String {
        state.engine.list_projects().await.unwrap()[0].id.clone()
    }

    #[tokio::test]
    async fn absent_or_null_params_decode_as_an_empty_object() {
        let (_data, state) = empty_state();
        for method in ["host.list", "dashboard.snapshot", "session.list"] {
            let response = dispatch(&state, RpcRequest::new(json!(1), method, Value::Null)).await;
            assert!(
                response.error.is_none(),
                "{method} with omitted params failed: {:?}",
                response.error
            );
        }
        // An explicit JSON object still decodes normally.
        let response = dispatch(&state, RpcRequest::new(json!(2), "session.list", json!({}))).await;
        assert!(response.error.is_none());

        // Malformed params keep their typed validation failure.
        let response = dispatch(&state, RpcRequest::new(json!(3), "host.list", json!(5))).await;
        let error = response.error.expect("scalar params must fail");
        assert_eq!(error.code, codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn http_auth_failure_uses_the_rpc_error_shape() {
        let (_data, state) = empty_state();
        let response = crate::http::rpc_http(
            State(state.clone()),
            header::HeaderMap::new(),
            HttpRequest::builder().body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer")
        );
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope["jsonrpc"], "2.0");
        assert_eq!(envelope["error"]["code"], codes::UNAUTHORIZED);
        assert_eq!(envelope["error"]["message"], "unauthorized");
        assert!(envelope["result"].is_null());

        // The wrong token is indistinguishable from a missing one.
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer wrong-token".parse().unwrap());
        let response = crate::http::rpc_http(
            State(state),
            headers,
            HttpRequest::builder().body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn http_event_subscribe_does_not_claim_success() {
        let (_data, state) = empty_state();
        let response = dispatch(
            &state,
            RpcRequest::new(json!(7), "event.subscribe", json!({})),
        )
        .await;
        let error = response.error.expect("HTTP subscribe must fail");
        assert_eq!(error.code, codes::METHOD_NOT_FOUND);
        let data = error.data.expect("typed hint");
        assert_eq!(data["code"], "UNSUPPORTED_TRANSPORT");
        assert_eq!(data["required_transport"], "ws");
    }

    #[tokio::test]
    async fn session_create_without_provider_reports_the_start_failure() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let response = dispatch(
            &state,
            RpcRequest::new(
                json!(8),
                "session.create",
                json!({"project_id": project, "goal": "needs a provider"}),
            ),
        )
        .await;
        let error = response.error.expect("a failed start must not be success");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        let data = error.data.expect("typed start failure");
        assert_eq!(data["code"], "SESSION_START_FAILED");
        assert_eq!(data["phase"], "agent.start");
        assert_eq!(data["retryable"], true);
        let session_id = data["session_id"].as_str().unwrap().to_string();

        // The session exists exactly once and records the unavailable agent.
        let sessions = state.engine.list_sessions(None).await.unwrap();
        assert_eq!(
            sessions.iter().filter(|row| row.id == session_id).count(),
            1
        );
        let events = state.engine.list_events(session_id, 0, 200).await.unwrap();
        assert!(
            events.iter().any(|event| event.kind == "agent.unavailable"),
            "missing agent.unavailable event"
        );
    }

    #[tokio::test]
    async fn session_create_validation_failure_keeps_a_typed_code() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let response = dispatch(
            &state,
            RpcRequest::new(
                json!(9),
                "session.create",
                json!({
                    "project_id": project,
                    "goal": "unlimited with a budget",
                    "unlimited": true,
                    "token_budget": 1_000,
                }),
            ),
        )
        .await;
        let error = response.error.expect("invalid budgets must fail");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        assert_eq!(
            error.data.expect("typed code")["code"],
            "SESSION_UNLIMITED_CONFLICT"
        );
        assert!(state.engine.list_sessions(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn operation_receipt_conflict_is_a_typed_rpc_error() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let session = state
            .engine
            .create_session(project, "operation receipt".to_string(), None)
            .await
            .unwrap();
        // A pending receipt for a different payload fingerprint makes the
        // core layer report `Claim::Conflict`, which must keep a stable code.
        let store = state.engine.store().clone();
        assert!(matches!(
            store
                .claim_session_operation(
                    &session.id,
                    "op-conflict",
                    "session.prompt",
                    "not-the-daemon-fingerprint",
                )
                .unwrap(),
            Claim::New
        ));
        let response = dispatch(
            &state,
            RpcRequest::new(
                json!(10),
                "session.prompt",
                json!({
                    "session_id": session.id,
                    "content": "hello",
                    "operation_id": "op-conflict",
                }),
            ),
        )
        .await;
        let error = response.error.expect("conflicting operation id must fail");
        assert_eq!(error.code, codes::CONFLICT);
        let data = error.data.expect("typed operation code");
        assert_eq!(data["code"], "OPERATION_CONFLICT");
        assert_eq!(data["retryable"], false);
    }

    #[test]
    fn operation_receipt_outcomes_keep_distinct_typed_codes() {
        let cases = [
            (
                "operation op-1 is already in progress for session ses-1; not starting a duplicate",
                codes::CONFLICT,
                "OPERATION_IN_PROGRESS",
                true,
            ),
            (
                "operation id op-1 was already used for session ses-1 with a different method or payload",
                codes::CONFLICT,
                "OPERATION_CONFLICT",
                false,
            ),
            (
                "operation op-1 has an unknown outcome after a daemon restart; inspect session ses-1 before retrying",
                codes::CONFLICT,
                "OPERATION_UNKNOWN",
                false,
            ),
            (
                "operation_id must not contain control characters",
                codes::INVALID_PARAMS,
                "INVALID_OPERATION_ID",
                false,
            ),
        ];
        for (message, rpc_code, code, retryable) in cases {
            let response = operation_error(Some(json!(1)), anyhow::anyhow!(message));
            let error = response.error.expect("error");
            assert_eq!(error.code, rpc_code, "{message}");
            let data = error.data.expect("typed data");
            assert_eq!(data["code"], code, "{message}");
            assert_eq!(data["retryable"], retryable, "{message}");
        }
        // An unrelated store failure stays INTERNAL_ERROR with no invented code.
        assert!(
            operation_receipt_error(Some(json!(1)), &anyhow::anyhow!("database is closed"))
                .is_none()
        );
        let response = operation_error(Some(json!(1)), anyhow::anyhow!("database is closed"));
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INTERNAL_ERROR);
        assert!(error.data.is_none());
    }

    #[test]
    fn policy_validation_failures_are_invalid_params() {
        let response = validation_error(
            Some(json!(1)),
            &anyhow::anyhow!("invalid critical-path policy: reserve_ratio must be 0..1"),
        )
        .expect("typed policy error");
        let error = response.error.expect("error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        assert_eq!(error.data.expect("data")["code"], "POLICY_INVALID");

        let response = validation_error(
            Some(json!(2)),
            &anyhow::anyhow!("gui grace_secs must be at most 600"),
        )
        .expect("typed gui policy error");
        assert_eq!(response.error.expect("error").code, codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn gui_policy_set_preserves_the_user_supplied_grace() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        for grace_secs in [0u64, 7] {
            let response = dispatch(
                &state,
                RpcRequest::new(
                    json!(11),
                    "gui.policy.set",
                    json!({
                        "project_id": project,
                        "policy": {"grace_secs": grace_secs},
                    }),
                ),
            )
            .await;
            assert!(response.error.is_none(), "{:?}", response.error);
            let result = response.result.unwrap();
            assert_eq!(
                result["policy"]["grace_secs"], grace_secs,
                "the effective policy must be the user-supplied value"
            );
            let stored = state.engine.get_gui_policy(&project).await.unwrap();
            assert_eq!(stored.grace_secs, grace_secs);
        }
        // An out-of-range value still fails validation; the policy is not
        // silently rewritten into range.
        let response = dispatch(
            &state,
            RpcRequest::new(
                json!(12),
                "gui.policy.set",
                json!({"project_id": project, "policy": {"grace_secs": 601}}),
            ),
        )
        .await;
        assert_eq!(
            response.error.expect("out of range").code,
            codes::INVALID_PARAMS
        );
    }

    #[tokio::test]
    async fn grace_sweep_does_not_release_an_in_flight_action() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let first = state
            .engine
            .create_session(project.clone(), "gui in flight".to_string(), None)
            .await
            .unwrap();
        let second = state
            .engine
            .create_session(project.clone(), "gui waiter".to_string(), None)
            .await
            .unwrap();
        // Zero grace is the worst case for the old sweeper.
        let policy = GuiPolicy {
            grace_secs: 0,
            wait_ms: 0,
            ..GuiPolicy::default()
        };
        state
            .engine
            .store()
            .set_gui_policy(&project, &policy)
            .unwrap();
        let ticket = match state
            .engine
            .gui_begin_action_with_policy(
                &policy,
                vp_core::gui::GuiSurface::Browser,
                &first.id,
                "agent-a",
                "Agent A",
            )
            .await
            .unwrap()
        {
            vp_core::gui::GuiAcquire::Acquired(ticket) => ticket,
            other => panic!("expected an acquired ticket, got {other:?}"),
        };
        tokio::time::sleep(Duration::from_millis(25)).await;

        // `spawn_gui_grace_sweeper` calls exactly this every second. It must
        // not release a holder whose action is still in flight.
        assert_eq!(state.engine.sweep_gui_grace().await, 0);
        let (holder, _) = state.engine.gui_lease_snapshot().await;
        assert_eq!(
            holder.expect("in-flight holder was released").session_id,
            first.id
        );
        let busy = state
            .engine
            .gui_begin_action_with_policy(
                &policy,
                vp_core::gui::GuiSurface::Computer,
                &second.id,
                "agent-b",
                "Agent B",
            )
            .await
            .unwrap();
        match busy {
            vp_core::gui::GuiAcquire::Busy(busy) => {
                assert_eq!(busy.code, "GUI_BUSY");
                assert_eq!(busy.holder_session_id.as_deref(), Some(first.id.as_str()));
            }
            vp_core::gui::GuiAcquire::Acquired(_) => {
                panic!("a second session acquired the GUI during an in-flight action")
            }
        }

        // After `finish`, zero grace releases normally and the other session
        // acquires with a new generation.
        state
            .engine
            .gui_finish_action(&ticket, vp_core::gui::GuiActionOutcome::new("snapshot"))
            .await;
        assert_eq!(state.engine.sweep_gui_grace().await, 1);
        let (holder, _) = state.engine.gui_lease_snapshot().await;
        assert!(holder.is_none());
        let next = match state
            .engine
            .gui_begin_action_with_policy(
                &policy,
                vp_core::gui::GuiSurface::Computer,
                &second.id,
                "agent-b",
                "Agent B",
            )
            .await
            .unwrap()
        {
            vp_core::gui::GuiAcquire::Acquired(ticket) => ticket,
            other => panic!("expected a zero-grace acquisition, got {other:?}"),
        };
        assert!(next.lease_generation > ticket.lease_generation);
        state.engine.gui_end_session(&second.id, "cleanup").await;
    }

    #[tokio::test]
    async fn stale_in_flight_ticket_does_not_block_after_the_lease_row_is_gone() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let first = state
            .engine
            .create_session(project.clone(), "stale ticket".to_string(), None)
            .await
            .unwrap();
        let second = state
            .engine
            .create_session(project, "stale ticket waiter".to_string(), None)
            .await
            .unwrap();
        let policy = GuiPolicy {
            grace_secs: 300,
            wait_ms: 0,
            ..GuiPolicy::default()
        };
        let ticket = match state
            .engine
            .gui_begin_action_with_policy(
                &policy,
                vp_core::gui::GuiSurface::Browser,
                &first.id,
                "agent-a",
                "Agent A",
            )
            .await
            .unwrap()
        {
            vp_core::gui::GuiAcquire::Acquired(ticket) => ticket,
            other => panic!("expected an acquired ticket, got {other:?}"),
        };
        // Simulate a killed action whose ticket is never finished, while the
        // SQLite lease is removed out from under the coordinator (operator
        // release or the store's own lease expiry).
        assert!(
            state
                .engine
                .release_computer_lease(&ticket.lease_id)
                .await
                .unwrap()
        );
        assert!(
            !state
                .engine
                .computer_lease_active(&ticket.lease_id)
                .await
                .unwrap()
        );

        // The stale in-memory ticket must not block a different session: the
        // existing `vanished` path reclaims the holder even though grace is
        // still 300s away.
        let next = match state
            .engine
            .gui_begin_action_with_policy(
                &policy,
                vp_core::gui::GuiSurface::Computer,
                &second.id,
                "agent-b",
                "Agent B",
            )
            .await
            .unwrap()
        {
            vp_core::gui::GuiAcquire::Acquired(ticket) => ticket,
            other => panic!("a vanished lease still blocked reclaim: {other:?}"),
        };
        assert_eq!(next.session_id, second.id);
        assert!(next.lease_generation > ticket.lease_generation);
        state.engine.gui_end_session(&second.id, "cleanup").await;
    }

    #[test]
    fn disabling_orchestration_keeps_lease_sweeps_active() {
        let env =
            |key: &str| (key == "VERLINPARK_ORCHESTRATION_TICK_SECS").then(|| "0".to_string());
        assert_eq!(
            sweep_interval_secs(env, "VERLINPARK_ORCHESTRATION_TICK_SECS", 20),
            None
        );
        assert_eq!(
            sweep_interval_secs(env, "VERLINPARK_RUNTIME_LEASE_SWEEP_SECS", 30),
            Some(30),
            "the lease sweep must have its own gate"
        );
        assert_eq!(
            sweep_interval_secs(
                |key| (key == "VERLINPARK_RUNTIME_LEASE_SWEEP_SECS").then(|| "0".to_string()),
                "VERLINPARK_RUNTIME_LEASE_SWEEP_SECS",
                30
            ),
            None
        );
    }

    #[tokio::test]
    async fn bind_failure_starts_no_background_work_and_writes_no_discovery() {
        let data = TempDir::new().unwrap();
        // A crashed daemon may have left a stale discovery file behind.
        std::fs::create_dir_all(data.path().join("run")).unwrap();
        std::fs::write(
            data.path().join("run").join("daemon.json"),
            r#"{"pid":1,"port":1,"token":"stale","protocol":1,"version":"0","started_at":"x"}"#,
        )
        .unwrap();
        // Occupy a loopback port, then ask the daemon to bind the same one.
        let occupied = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = occupied.local_addr().unwrap().port();
        let error = crate::run_daemon(data.path().to_path_buf(), Some(port))
            .await
            .expect_err("a bound port must fail startup");
        assert!(
            format!("{error:#}").contains("binding 127.0.0.1"),
            "{error:#}"
        );
        assert!(
            !data.path().join("run").join("daemon.json").exists(),
            "a failed bind must not leave a discovery file pointing at a dead port"
        );
        assert!(
            !data.path().join("run").join("daemon.json.tmp").exists(),
            "a failed bind must not leave a discovery temp file"
        );
        // No session work was created by the aborted startup.
        let store = Store::open(data.path()).unwrap();
        assert!(store.list_projects().unwrap().is_empty());
    }

    async fn budget_call(
        state: &AppState,
        method: &str,
        params: Value,
    ) -> vp_protocol::rpc::RpcResponse {
        dispatch(state, RpcRequest::new(json!(1), method, params)).await
    }

    #[tokio::test]
    async fn session_budget_rpcs_round_trip_and_preserve_partial_updates() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let session = state
            .engine
            .create_session(project.clone(), "budget rpc".into(), Some(1_000))
            .await
            .unwrap();

        let response = budget_call(
            &state,
            "session.budget.get",
            json!({"session_id": session.id}),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let status = response.result.unwrap();
        assert_eq!(status["mode"], "fixed");
        assert_eq!(status["effective_token_budget"], 1_000);
        assert_eq!(status["spent_tokens"], 0);
        assert_eq!(status["forecast"]["source"], "unavailable");

        // A synthetic forecast preview is read-only and bounded.
        let response = budget_call(
            &state,
            "session.budget.forecast",
            json!({"session_id": session.id}),
        )
        .await;
        let forecast = response.result.unwrap();
        assert_eq!(forecast["session_id"], session.id);
        assert_eq!(forecast["forecast"]["sample_calls"], 0);

        // Only the hard cap is supplied; the effective token budget must stay.
        let response = budget_call(
            &state,
            "session.budget.set",
            json!({"session_id": session.id, "hard_token_cap": 5_000}),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let budget = response.result.unwrap()["budget"].clone();
        assert_eq!(budget["mode"], "fixed");
        assert_eq!(budget["effective_token_budget"], 1_000);
        assert_eq!(budget["hard_token_cap"], 5_000);

        // Switching to auto keeps the existing hard cap and effective budget.
        let response = budget_call(
            &state,
            "session.budget.set",
            json!({"session_id": session.id, "mode": "auto"}),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let budget = response.result.unwrap()["budget"].clone();
        assert_eq!(budget["mode"], "auto");
        assert_eq!(budget["hard_token_cap"], 5_000);
        assert_eq!(budget["effective_token_budget"], 1_000);

        // A later effective-budget update must not clear the auto cap or mode.
        let response = budget_call(
            &state,
            "session.budget.set",
            json!({"session_id": session.id, "token_budget": 2_000}),
        )
        .await;
        let budget = response.result.unwrap()["budget"].clone();
        assert_eq!(budget["mode"], "auto");
        assert_eq!(budget["hard_token_cap"], 5_000);
        assert_eq!(budget["effective_token_budget"], 2_000);

        let response = budget_call(
            &state,
            "project.auto_budget.get",
            json!({"project_id": project}),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let policy = response.result.unwrap();
        assert_eq!(policy["policy"]["enabled"], false);
        assert!(policy["updated_at"].is_null());

        let response = budget_call(
            &state,
            "project.auto_budget.set",
            json!({
                "project_id": project,
                "policy": {"enabled": true, "extend_ratio": 2.0},
            }),
        )
        .await;
        let error = response.error.expect("invalid ratio must fail");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("typed policy code");
        assert_eq!(data["code"], "INVALID_AUTO_BUDGET_POLICY");

        let response = budget_call(
            &state,
            "project.auto_budget.set",
            json!({
                "project_id": project,
                "policy": {"enabled": true, "hard_cost_cap_usd": 5.0, "min_runway_turns": 2},
            }),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let policy = response.result.unwrap();
        assert_eq!(policy["policy"]["enabled"], true);
        assert_eq!(policy["policy"]["hard_cost_cap_usd"], 5.0);
        assert_eq!(policy["policy"]["min_runway_turns"], 2);
        assert!(policy["updated_at"].is_string());
    }

    #[tokio::test]
    async fn project_run_policy_rpcs_round_trip_and_validate() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;

        let response = budget_call(
            &state,
            "project.run_policy.get",
            json!({"project_id": project}),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let default = response.result.unwrap();
        assert_eq!(default["policy"]["default_profile"], "balanced");
        assert_eq!(default["policy"]["completion_reserve_ratio"], 0.20);
        assert_eq!(default["policy"]["attention_cap"], 20);
        assert!(default["updated_at"].is_null());

        let response = budget_call(
            &state,
            "project.run_policy.set",
            json!({
                "project_id": project,
                "policy": {"completion_reserve_ratio": 0.75},
            }),
        )
        .await;
        let error = response.error.expect("invalid reserve must fail");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("typed policy code");
        assert_eq!(data["code"], "INVALID_BUDGET");

        let response = budget_call(
            &state,
            "project.run_policy.set",
            json!({
                "project_id": project,
                "policy": {
                    "default_profile": "fast",
                    "completion_reserve_ratio": 0.30,
                    "attention_cap": 9,
                    "stop_policy": {"no_progress_turns": 4}
                },
            }),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let saved = response.result.unwrap();
        assert_eq!(saved["policy"]["default_profile"], "fast");
        assert_eq!(saved["policy"]["completion_reserve_ratio"], 0.30);
        assert_eq!(saved["policy"]["attention_cap"], 9);
        assert_eq!(saved["policy"]["stop_policy"]["no_progress_turns"], 4);
        assert!(saved["updated_at"].is_string());

        let response = budget_call(
            &state,
            "project.run_policy.get",
            json!({"project_id": project}),
        )
        .await;
        let stored = response.result.unwrap();
        assert_eq!(stored["policy"]["default_profile"], "fast");
        assert_eq!(stored["policy"]["attention_cap"], 9);
        assert_eq!(stored["updated_at"], saved["updated_at"]);
    }

    #[tokio::test]
    async fn session_budget_set_keeps_legacy_unlimited_requests_working() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let session = state
            .engine
            .create_session(project, "legacy unlimited".into(), Some(1_000))
            .await
            .unwrap();

        let response = budget_call(
            &state,
            "session.budget.set",
            json!({"session_id": session.id, "unlimited": true}),
        )
        .await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let budget = response.result.unwrap()["budget"].clone();
        assert_eq!(budget["mode"], "unlimited");
        assert_eq!(budget["unlimited"], true);
        // Effective limits stay recorded for a later restore.
        assert_eq!(budget["effective_token_budget"], 1_000);

        let response = budget_call(
            &state,
            "session.budget.set",
            json!({"session_id": session.id, "unlimited": false}),
        )
        .await;
        let budget = response.result.unwrap()["budget"].clone();
        assert_eq!(budget["mode"], "fixed");
        assert_eq!(budget["unlimited"], false);
    }

    #[tokio::test]
    async fn auto_budget_set_without_a_cap_fails_with_a_typed_error() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let session = state
            .engine
            .create_session(project, "auto no cap".into(), Some(1_000))
            .await
            .unwrap();

        let response = budget_call(
            &state,
            "session.budget.set",
            json!({"session_id": session.id, "mode": "auto"}),
        )
        .await;
        let error = response.error.expect("auto without a cap must fail");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("typed auto-budget code");
        assert_eq!(data["code"], "AUTO_BUDGET_CAP_REQUIRED");
        assert_eq!(data["retryable"], false);
    }

    #[tokio::test]
    async fn session_create_auto_without_a_cap_fails_before_any_session_exists() {
        let (_data, _repo, state) = project_state().await;
        let project = first_project_id(&state).await;
        let response = budget_call(
            &state,
            "session.create",
            json!({
                "project_id": project,
                "goal": "auto without a cap",
                "budget_mode": "auto",
            }),
        )
        .await;
        let error = response.error.expect("auto creation must fail closed");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("typed auto-budget code");
        assert_eq!(data["code"], "AUTO_BUDGET_POLICY_REQUIRED");
        assert!(
            state
                .engine
                .list_sessions(Some(project))
                .await
                .unwrap()
                .is_empty(),
            "a rejected auto creation must not leave a session or worktree"
        );
    }

    #[tokio::test]
    async fn imported_sessions_and_missing_projects_keep_typed_budget_errors() {
        let (_data, _repo, app) = project_state().await;
        let project = first_project_id(&app).await;
        let session = app
            .engine
            .create_session(project, "imported budget".into(), Some(1_000))
            .await
            .unwrap();
        let bundle = app.engine.export_session(&session.id).await.unwrap();
        let imported_data = TempDir::new().unwrap();
        let imported_engine = Engine::new(Store::open(imported_data.path()).unwrap());
        imported_engine.import_session(bundle).await.unwrap();
        let imported_state = state(
            &imported_data,
            imported_engine.clone(),
            AgentService::unconfigured(imported_engine.clone()),
        );
        let response = budget_call(
            &imported_state,
            "session.budget.set",
            json!({"session_id": session.id, "mode": "fixed"}),
        )
        .await;
        let error = response.error.expect("imported session must be read-only");
        assert_eq!(error.code, codes::CONFLICT);
        assert_eq!(
            error.data.expect("typed read-only code")["code"],
            "SESSION_IMPORTED_READONLY"
        );

        let response = budget_call(
            &app,
            "project.auto_budget.get",
            json!({"project_id": "prj-missing"}),
        )
        .await;
        let error = response.error.expect("missing project must fail");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.data.expect("typed project code")["code"],
            "PROJECT_NOT_FOUND"
        );
    }

    #[test]
    fn budget_validation_errors_map_to_typed_invalid_params() {
        let error: anyhow::Error = vp_protocol::types::BudgetValidationError::new(
            vp_protocol::types::auto_budget_codes::BUDGET_BELOW_SPENT,
            "token budget 10 is below tokens already spent (20)",
        )
        .into();
        let response = crate::errors::budget_error(Some(json!(1)), error);
        let error = response.error.expect("typed budget error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        let data = error.data.expect("typed data");
        assert_eq!(data["code"], "BUDGET_BELOW_SPENT");
        assert_eq!(data["retryable"], false);

        // A store path that stringifies a policy validation keeps the stable
        // policy code instead of degrading to INTERNAL_ERROR.
        let response = crate::errors::budget_error(
            Some(json!(2)),
            anyhow::anyhow!("invalid auto-budget policy: extend_ratio must be in (0, 1]"),
        );
        let error = response.error.expect("typed policy error");
        assert_eq!(error.code, codes::INVALID_PARAMS);
        assert_eq!(
            error.data.expect("typed data")["code"],
            "INVALID_AUTO_BUDGET_POLICY"
        );
    }
}

#[path = "agent_operator_tests.rs"]
mod agent_operator_rpc_tests;

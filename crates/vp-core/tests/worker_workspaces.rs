//! S4 Worker workspace and two-level integration tests.
//!
//! The tests exercise the real `Engine` store/Git path and, for the end-to-end
//! case, the real `AgentService` loop with a fake provider. They assert that
//! Labour commits merge into the Worker session workspace, that the Worker
//! commit still enters the normal Verifier/Integrator, and that a failed
//! internal merge leaves the Worker HEAD unchanged.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use tempfile::TempDir;
use vp_core::Engine;
use vp_core::agent::{AgentConfig, AgentService, CapacityController, ScopeLocks};
use vp_core::tools::ToolRegistry;
use vp_protocol::types::{
    AgentIdentityPolicy, AgentWorkspaceEnsureParams, AgentWorkspaceLandParams,
    AgentWorkspaceListParams, ApprovalMode, CreateAgentIdentityParams, RoleModels,
    SessionAssignParams,
};
use vp_providers::{ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall};
use vp_store::Store;

struct QueueProvider {
    responses: Mutex<VecDeque<Vec<StreamEvent>>>,
}

#[async_trait]
impl Provider for QueueProvider {
    fn id(&self) -> &str {
        "worker-workspace-queue"
    }

    async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::TextDelta("done".into()), StreamEvent::Done]);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

fn run(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "workspace integration\n").unwrap();
    run(path, &["init", "-q"]);
    run(path, &["config", "user.email", "workspace@test.local"]);
    run(path, &["config", "user.name", "Workspace Test"]);
    run(path, &["add", "."]);
    run(path, &["commit", "-q", "-m", "initial"]);
}

fn service(
    dir: &Path,
    provider: Arc<dyn Provider>,
) -> (Engine, AgentService, vp_protocol::types::Project) {
    let store = Store::open(dir).unwrap();
    let engine = Engine::new(store);
    let repo = dir.join("repo");
    init_repo(&repo);
    let project = engine
        .store()
        .add_project(repo.to_str().unwrap(), Some("worker workspace"))
        .unwrap();
    let config = AgentConfig {
        provider,
        provider_name: "openai-compatible".to_string(),
        model: "test-model".to_string(),
        role_models: RoleModels::default(),
        base_url: None,
        max_turns: 3,
        max_tokens: Some(256),
        max_managers: 2,
        max_workers: 3,
        max_labour: 3,
        auto_expand_workers: false,
        integration_command: None,
        tools: ToolRegistry::default_tools(),
        max_active_sessions: 2,
        scope_locks: Arc::new(ScopeLocks::with_store(engine.store().clone())),
        capacity: Arc::new(CapacityController::new(6, 6)),
        provider_idle_timeout_secs: 30,
        question_timeout_secs: 60,
        approval_mode: ApprovalMode::default(),
        approval_timeout_secs: 60,
        compact_after_tokens: 0,
        compact_keep_messages: 8,
        compact_keep_tokens: 1_000_000,
        compact_prepare_ratio: 0.6,
        context_window: 0,
    };
    let service = AgentService::configured(engine.clone(), config);
    (engine, service, project)
}

fn head(path: &Path) -> String {
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

fn branch_head(repo: &Path, branch: &str) -> String {
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", branch])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

async fn enable_identity_policy(engine: &Engine, project_id: &str) {
    engine
        .set_agent_identity_policy(
            project_id,
            AgentIdentityPolicy {
                enabled: true,
                ..AgentIdentityPolicy::default()
            },
        )
        .await
        .unwrap();
}

async fn create_worker_identity(
    engine: &Engine,
    project_id: &str,
    component: &str,
    title: &str,
) -> String {
    engine
        .create_agent_identity(CreateAgentIdentityParams {
            project_id: project_id.to_string(),
            role: "worker".to_string(),
            title: Some(title.to_string()),
            department: Some("Platform".to_string()),
            component: Some(component.to_string()),
            profile: Some("coding".to_string()),
            model: Some("test-model".to_string()),
            ..CreateAgentIdentityParams::default()
        })
        .await
        .unwrap()
        .identity
        .id
}

async fn assign(engine: &Engine, session_id: &str, identity_id: &str) {
    engine
        .assign_session_identity(SessionAssignParams {
            session_id: session_id.to_string(),
            identity_id: identity_id.to_string(),
            ..SessionAssignParams::default()
        })
        .await
        .unwrap();
}

async fn workspace_fixture() -> (
    TempDir,
    Engine,
    AgentService,
    vp_protocol::types::Project,
    vp_protocol::types::Session,
    String,
    String,
) {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, service, project) = service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity = create_worker_identity(&engine, &project.id, "auth", "Auth Worker").await;
    let session = engine
        .create_session(project.id.clone(), "workspace goal".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity).await;
    let project_id = project.id.clone();
    (dir, engine, service, project, session, identity, project_id)
}

#[tokio::test]
async fn assigned_worker_gets_component_branch_and_session_worktree() {
    let (_dir, engine, _service, project, session, identity, _project_id) =
        workspace_fixture().await;
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    assert_eq!(binding.workspace.identity_id, identity);
    assert_eq!(binding.workspace.component, "auth");
    assert_eq!(binding.session.session_id, session.id);
    assert_eq!(binding.session.status, "active");
    assert!(
        vp_core::worktree::branch_exists(Path::new(&project.root), &binding.workspace.branch)
            .unwrap()
    );
    assert!(
        vp_core::worktree::branch_exists(Path::new(&project.root), &binding.session.branch)
            .unwrap()
    );
    assert!(
        Path::new(&binding.session.worktree_path)
            .join(".git")
            .is_file()
    );
    assert_eq!(
        vp_core::worktree::current_branch(Path::new(&binding.session.worktree_path))
            .unwrap()
            .trim(),
        binding.session.branch
    );

    // A Labour child worktree branches from the Worker workspace HEAD, not the
    // session branch.
    let worker_head = head(Path::new(&binding.session.worktree_path));
    let labour = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            "labour-task",
            &binding.session.branch,
        )
        .await
        .unwrap();
    assert_eq!(labour.base_commit, worker_head);
    assert_eq!(
        vp_core::worktree::head_commit(&labour.path).unwrap(),
        worker_head
    );

    let listed = engine
        .list_agent_workspaces(AgentWorkspaceListParams {
            project_id: Some(project.id.clone()),
            ..AgentWorkspaceListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.workspaces.len(), 1);
    assert_eq!(listed.sessions.len(), 1);
    assert!(listed.workspaces[0].lease_active);
}

#[tokio::test]
async fn labour_commit_merges_into_workspace_and_conflict_rolls_back() {
    let (_dir, engine, _service, project, session, _identity, _project_id) =
        workspace_fixture().await;
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    let worker = Path::new(&binding.session.worktree_path);
    let worker_head = head(worker);

    // Create a Labour worktree and commit a file inside the declared scope.
    let labour = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            "labour-1",
            &binding.session.branch,
        )
        .await
        .unwrap();
    std::fs::write(labour.path.join("feature.txt"), "from labour\n").unwrap();
    run(&labour.path, &["add", "feature.txt"]);
    run(&labour.path, &["commit", "-q", "-m", "labour feature"]);
    let labour_commit = head(&labour.path);

    let outcome = engine
        .merge_labour_commit(
            &binding.workspace.id,
            &session.id,
            "labour-1",
            Some("T1"),
            &labour_commit,
            &["feature.txt".to_string()],
        )
        .await
        .unwrap();
    assert!(outcome.merged, "{}", outcome.detail);
    assert_eq!(outcome.pre_merge, worker_head);
    assert_eq!(
        std::fs::read_to_string(worker.join("feature.txt")).unwrap(),
        "from labour\n"
    );
    let merged_head = head(worker);
    let events = engine
        .store()
        .list_agent_workspace_events_for_project(&binding.workspace.project_id, None)
        .unwrap();
    let labour_event = events
        .iter()
        .find(|event| event.kind == "agent.workspace.labour_merged")
        .expect("labour merged event");
    assert_eq!(labour_event.payload["merge"]["merged"], true);
    assert_eq!(labour_event.payload["merge"]["commit"], labour_commit);
    assert!(
        !labour_event.payload.to_string().contains("from labour"),
        "event must not carry file content"
    );

    // A second Labour run changes the same file; the Worker workspace changes
    // it too, so the internal merge conflicts and must roll back exactly.
    let labour2 = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            "labour-2",
            &binding.session.branch,
        )
        .await
        .unwrap();
    std::fs::write(labour2.path.join("feature.txt"), "labour conflict\n").unwrap();
    run(&labour2.path, &["add", "feature.txt"]);
    run(&labour2.path, &["commit", "-q", "-m", "labour conflict"]);
    let conflict_commit = head(&labour2.path);
    std::fs::write(worker.join("feature.txt"), "worker change\n").unwrap();
    run(worker, &["add", "feature.txt"]);
    run(worker, &["commit", "-q", "-m", "worker change"]);
    let pre_conflict = head(worker);
    let outcome = engine
        .merge_labour_commit(
            &binding.workspace.id,
            &session.id,
            "labour-2",
            Some("T2"),
            &conflict_commit,
            &["feature.txt".to_string()],
        )
        .await
        .unwrap();
    assert!(!outcome.merged);
    assert_eq!(outcome.pre_merge, pre_conflict);
    assert_eq!(outcome.post_merge, pre_conflict);
    assert_eq!(head(worker), pre_conflict);
    assert_eq!(
        std::fs::read_to_string(worker.join("feature.txt")).unwrap(),
        "worker change\n"
    );
    assert_ne!(merged_head, outcome.pre_merge);
}

#[tokio::test]
async fn operator_ensure_without_session_does_not_hold_a_lease() {
    let (_dir, engine, _service, project, session, identity, _project_id) =
        workspace_fixture().await;
    let ensured = engine
        .ensure_agent_workspace(AgentWorkspaceEnsureParams {
            project_id: project.id.clone(),
            identity_id: identity.clone(),
            component: Some("auth".to_string()),
            ..AgentWorkspaceEnsureParams::default()
        })
        .await
        .unwrap();
    assert!(!ensured.workspace.lease_active);
    assert!(ensured.session.is_none());
    // A session attach can now acquire the lease without an operator release.
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    assert_eq!(binding.workspace.id, ensured.workspace.id);
    assert!(binding.workspace.lease_active);
}

#[tokio::test]
async fn two_sessions_cannot_lease_one_component_workspace() {
    let (dir, engine, _service, project, session, _identity, _project_id) =
        workspace_fixture().await;
    let _first = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("first binding");

    // A second identity on the same component, in a second session, cannot
    // acquire the component lease while the first is live.
    let second_identity =
        create_worker_identity(&engine, &project.id, "auth", "Second Worker").await;
    let second_session = engine
        .create_session(project.id.clone(), "second goal".into(), None)
        .await
        .unwrap();
    assign(&engine, &second_session.id, &second_identity).await;
    let error = engine
        .ensure_agent_workspace(AgentWorkspaceEnsureParams {
            project_id: project.id.clone(),
            identity_id: second_identity,
            component: Some("auth".to_string()),
            session_id: Some(second_session.id.clone()),
            ..AgentWorkspaceEnsureParams::default()
        })
        .await
        .unwrap_err();
    let typed = error
        .downcast_ref::<vp_protocol::types::AgentWorkspaceError>()
        .expect("typed workspace error");
    assert_eq!(
        typed.code,
        vp_protocol::types::agent_workspace_codes::WORKSPACE_BUSY
    );
    // The fallback used by a run does not fail the whole session.
    let fallback = engine
        .attach_worker_workspace_for_run(&second_session.id, None, Some("Platform"))
        .await
        .unwrap();
    assert!(fallback.is_none());
    drop(dir);
}

#[tokio::test]
async fn expired_lease_recovery_and_explicit_release() {
    let (_dir, engine, _service, _project, session, identity, _project_id) =
        workspace_fixture().await;
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    let future = time::OffsetDateTime::now_utc() + time::Duration::seconds(7_200);
    let events = engine
        .store()
        .expire_agent_workspace_leases_at(future, Some(10))
        .unwrap();
    assert_eq!(events.len(), 1);
    let released = engine
        .store()
        .get_session_worker_workspace(&session.id, &identity)
        .unwrap()
        .unwrap();
    assert_eq!(released.status, "released");
    // Explicit release is idempotent after expiry.
    assert_eq!(
        engine
            .release_session_workspaces(&session.id, "session_ended")
            .await
            .unwrap(),
        0
    );
    assert_eq!(binding.session.id, released.id);
}

#[tokio::test]
async fn land_requires_accepted_commits_and_explicit_confirmation() {
    let (dir, engine, _service, project, session, _identity, _project_id) =
        workspace_fixture().await;
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    let worker = Path::new(&binding.session.worktree_path);
    std::fs::write(worker.join("landed.txt"), "landed\n").unwrap();
    run(worker, &["add", "landed.txt"]);
    run(worker, &["commit", "-q", "-m", "worker commit"]);
    let worker_commit = head(worker);
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();

    // Not accepted by the session branch yet: fail closed.
    let error = engine
        .land_agent_workspace(AgentWorkspaceLandParams {
            workspace_id: binding.workspace.id.clone(),
            session_id: Some(session.id.clone()),
            confirm: true,
            ..AgentWorkspaceLandParams::default()
        })
        .await
        .unwrap_err();
    let typed = error
        .downcast_ref::<vp_protocol::types::AgentWorkspaceError>()
        .unwrap();
    assert_eq!(
        typed.code,
        vp_protocol::types::agent_workspace_codes::WORKSPACE_NOT_ACCEPTED
    );

    // Simulate the deterministic Integrator merging the exact commit into the
    // session branch.
    let session_worktree = Path::new(session.worktree.as_deref().unwrap());
    vp_core::worktree::merge_commit(session_worktree, &worker_commit, "verify+integrate").unwrap();
    let result = engine
        .land_agent_workspace(AgentWorkspaceLandParams {
            workspace_id: binding.workspace.id.clone(),
            session_id: Some(session.id.clone()),
            confirm: true,
            operator: Some("test".to_string()),
        })
        .await
        .unwrap();
    assert!(!result.already_up_to_date);
    assert!(
        vp_core::worktree::is_ancestor(
            Path::new(&project.root),
            &worker_commit,
            &result.landed_commit,
        )
        .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&result.workspace.worktree_path).join("landed.txt"))
            .unwrap(),
        "landed\n"
    );
    assert_eq!(
        branch_head(Path::new(&project.root), &binding.workspace.branch),
        result.landed_commit
    );
    assert!(
        vp_core::worktree::is_ancestor(
            Path::new(&project.root),
            &worker_commit,
            &binding.workspace.branch,
        )
        .unwrap()
    );
    let events = engine
        .store()
        .list_agent_workspace_events_for_project(&project.id, None)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.workspace.landed")
    );
    drop(dir);
}

#[tokio::test]
async fn archive_preserves_evidence_and_requires_release() {
    let (_dir, engine, _service, project, session, _identity, _project_id) =
        workspace_fixture().await;
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    let error = engine
        .archive_agent_workspace(
            vp_protocol::types::AgentWorkspaceArchiveParams {
                workspace_id: binding.workspace.id.clone(),
                reason: Some("done".to_string()),
                ..vp_protocol::types::AgentWorkspaceArchiveParams::default()
            },
            false,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("WORKSPACE_BUSY"),
        "archive must require release: {error}"
    );
    engine
        .release_session_workspaces(&session.id, "session_ended")
        .await
        .unwrap();
    let archived = engine
        .archive_agent_workspace(
            vp_protocol::types::AgentWorkspaceArchiveParams {
                workspace_id: binding.workspace.id.clone(),
                reason: Some("done".to_string()),
                ..vp_protocol::types::AgentWorkspaceArchiveParams::default()
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(archived.workspace.status, "archived");
    assert!(
        vp_core::worktree::branch_exists(Path::new(&project.root), &binding.workspace.branch)
            .unwrap(),
        "branch evidence must be preserved"
    );
    // Evidence is preserved: the branch still exists and the event remains.
    assert!(
        engine
            .store()
            .get_agent_workspace(&binding.workspace.id)
            .unwrap()
            .is_some()
    );
    let events = engine
        .store()
        .list_agent_workspace_events_for_project(&project.id, None)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "agent.workspace.archived")
    );
}

#[tokio::test]
async fn legacy_session_without_assignment_has_no_workspace() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let (engine, _service, project) = service(dir.path(), provider);
    let session = engine
        .create_session(project.id.clone(), "legacy".into(), None)
        .await
        .unwrap();
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, None)
        .await
        .unwrap();
    assert!(binding.is_none());
    let info = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            "legacy-task",
            session.branch.as_deref().unwrap_or("main"),
        )
        .await
        .unwrap();
    assert!(info.path.join(".git").is_file());
    let listed = engine
        .list_agent_workspaces(AgentWorkspaceListParams {
            project_id: Some(project.id),
            ..AgentWorkspaceListParams::default()
        })
        .await
        .unwrap();
    assert!(listed.workspaces.is_empty());
}

#[tokio::test]
async fn replay_repairs_workspace_projection_and_bundle_keeps_evidence() {
    let (_dir, engine, _service, project, session, identity, _project_id) =
        workspace_fixture().await;
    let binding = engine
        .attach_worker_workspace_for_run(&session.id, None, Some("Platform"))
        .await
        .unwrap()
        .expect("workspace binding");
    let report = engine
        .replay_agent_workspaces(vp_protocol::types::AgentWorkspaceReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);

    // Simulate projection loss and repair from the event stream.
    engine
        .store()
        .replace_agent_workspace_projection(&project.id, &[], &[])
        .unwrap();
    let drifted = engine
        .replay_agent_workspaces(vp_protocol::types::AgentWorkspaceReplayParams {
            project_id: project.id.clone(),
            repair: false,
        })
        .await
        .unwrap();
    assert!(!drifted.consistent);
    let repaired = engine
        .replay_agent_workspaces(vp_protocol::types::AgentWorkspaceReplayParams {
            project_id: project.id.clone(),
            repair: true,
        })
        .await
        .unwrap();
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    assert_eq!(
        engine
            .store()
            .get_session_worker_workspace(&session.id, &identity)
            .unwrap()
            .unwrap()
            .id,
        binding.session.id
    );

    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.agent_workspaces.len(), 1);
    assert_eq!(bundle.session_worker_workspaces.len(), 1);
    assert!(bundle.agent_workspaces[0].origin == "local");

    let import_dir = TempDir::new().unwrap();
    let import_store = Store::open(import_dir.path()).unwrap();
    let import_engine = Engine::new(import_store);
    import_engine.import_session(bundle).await.unwrap();
    let imported = import_engine
        .store()
        .list_agent_workspaces_for_replay(&project.id)
        .unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].origin, "imported");
    assert!(!imported[0].lease_active);
    let imported_binding = import_engine
        .store()
        .list_session_workspaces_for_replay(&project.id)
        .unwrap();
    assert_eq!(imported_binding.len(), 1);
    assert_eq!(imported_binding[0].status, "released");
    // Imported evidence is read-only.
    let error = import_engine
        .ensure_agent_workspace(AgentWorkspaceEnsureParams {
            project_id: project.id.clone(),
            identity_id: identity,
            ..AgentWorkspaceEnsureParams::default()
        })
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("WORKSPACE_IMPORTED_READONLY")
            || error.to_string().contains("WORKSPACE_IDENTITY_INVALID")
            || error.to_string().contains("WORKSPACE_NOT_FOUND"),
        "unexpected imported workspace error: {error}"
    );
}

#[tokio::test]
async fn worker_commit_still_enters_verifier_and_integrator_after_labour_merge() {
    let dir = TempDir::new().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
        responses: Mutex::new(VecDeque::from(vec![
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "leader_manager".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "manager",
                        "title": "Platform",
                        "objective": "own the platform department"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "manager_worker".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "worker",
                        "objective": "implement the feature",
                        "write_scope": ["feature.txt"]
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "worker_labour".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({
                        "role": "labour",
                        "objective": "write the feature"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "labour_write".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "feature.txt",
                        "content": "from labour\n"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("labour done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("worker done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::ToolCall(ToolCall {
                    id: "verdict".into(),
                    name: "submit_verdict".into(),
                    arguments: serde_json::json!({
                        "approved": true,
                        "summary": "diff and tests accepted"
                    })
                    .to_string(),
                }),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("manager done".into()),
                StreamEvent::Done,
            ],
            vec![
                StreamEvent::TextDelta("leader done".into()),
                StreamEvent::Done,
            ],
        ])),
    });
    let (engine, service, project) = service(dir.path(), provider);
    enable_identity_policy(&engine, &project.id).await;
    let identity = create_worker_identity(&engine, &project.id, "auth", "Auth Worker").await;
    let session = engine
        .create_session(project.id.clone(), "two-level integration".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test-model")
        .await
        .unwrap();
    assign(&engine, &session.id, &identity).await;
    service
        .start(&session.id, "two-level integration")
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let current = engine.store().get_session(&session.id).unwrap().unwrap();
        if matches!(current.status.as_str(), "idle" | "failed" | "cancelled") {
            assert_ne!(current.status, "failed", "agent run failed");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session did not settle: {}",
            current.status
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let events = engine
        .store()
        .list_agent_workspace_events_for_project(&project.id, None)
        .unwrap();
    let labour_events = events
        .iter()
        .filter(|event| event.kind == "agent.workspace.labour_merged")
        .collect::<Vec<_>>();
    assert_eq!(labour_events.len(), 1, "one internal Labour merge");
    assert_eq!(labour_events[0].payload["merge"]["merged"], true);
    let workspace = engine
        .list_agent_workspaces(AgentWorkspaceListParams {
            project_id: Some(project.id.clone()),
            ..AgentWorkspaceListParams::default()
        })
        .await
        .unwrap()
        .workspaces;
    assert_eq!(workspace.len(), 1);
    let bindings = engine
        .session_workspace(vp_protocol::types::SessionWorkspaceParams {
            session_id: session.id.clone(),
        })
        .await
        .unwrap()
        .bindings;
    assert_eq!(bindings.len(), 1);
    let workspace_head = branch_head(Path::new(&project.root), &bindings[0].branch);
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue.len(), 1, "one Worker enqueue, no double merge");
    assert_eq!(queue[0].status, "merged");
    assert_eq!(queue[0].commit, workspace_head);
    assert!(
        vp_core::worktree::is_ancestor(
            Path::new(session.worktree.as_deref().unwrap()),
            &workspace_head,
            &session.branch.clone().unwrap(),
        )
        .unwrap()
    );
    let session_branch = session.branch.unwrap();
    let session_head = branch_head(Path::new(&project.root), &session_branch);
    assert_eq!(
        std::fs::read_to_string(
            Path::new(session.worktree.as_deref().unwrap()).join("feature.txt")
        )
        .unwrap(),
        "from labour\n"
    );
    assert_ne!(
        session_head, workspace_head,
        "Integrator created a merge commit"
    );
}

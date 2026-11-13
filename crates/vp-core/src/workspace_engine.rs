//! S4 Worker workspace engine surface.
//!
//! The Git primitives live in `agent_workspace`; this module binds them to the
//! persistent store, session assignment, project policy, and event stream. It
//! never starts a provider call, bypasses an approval, or writes outside the
//! existing path jail/sandbox.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use vp_protocol::types::{
    AgentWorkspace, AgentWorkspaceArchiveParams, AgentWorkspaceEnsureParams,
    AgentWorkspaceEnsureResult, AgentWorkspaceGetResult, AgentWorkspaceLandParams,
    AgentWorkspaceLandResult, AgentWorkspaceListParams, AgentWorkspaceListResult,
    AgentWorkspaceParams, AgentWorkspaceReplayParams, AgentWorkspaceReplayReport,
    AgentWorkspaceResult, AgentWorkspaceRetireParams, SessionWorkerWorkspace,
    SessionWorkspaceParams, SessionWorkspaceResult, agent_workspace_codes,
};

use crate::Engine;
use crate::agent_workspace;

/// One explicit Worker workspace binding used by a run. It is in-memory run
/// metadata; the durable rows and events live in the project workspace store.
#[derive(Debug, Clone)]
pub struct WorkerWorkspaceBinding {
    pub workspace: AgentWorkspace,
    pub session: SessionWorkerWorkspace,
}

impl Engine {
    // -----------------------------------------------------------------
    // S4 Worker workspaces and two-level integration
    // -----------------------------------------------------------------

    pub async fn list_agent_workspaces(
        &self,
        params: AgentWorkspaceListParams,
    ) -> Result<AgentWorkspaceListResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let project_id = match params.project_id.as_deref() {
                Some(project_id) => Some(project_id.to_string()),
                None => match params.session_id.as_deref() {
                    Some(session_id) => store
                        .get_session(session_id)?
                        .map(|session| session.project_id),
                    None => None,
                },
            };
            if let Some(status) = params.status.as_deref()
                && !vp_protocol::types::AGENT_WORKSPACE_STATUSES.contains(&status)
            {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_INVALID,
                    format!("workspace status must be active, archived, or retired: {status}"),
                ));
            }
            if project_id.is_none() && params.identity_id.is_none() && params.session_id.is_none() {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_INVALID,
                    "list workspaces requires a project, identity, or session filter",
                ));
            }
            let sessions = match params.session_id.as_deref() {
                Some(session_id) => store.list_session_worker_workspaces(session_id)?,
                None => match project_id.as_deref() {
                    Some(project_id) => store.list_session_workspaces_for_replay(project_id)?,
                    None => Vec::new(),
                },
            };
            let workspaces = if params.session_id.is_some() {
                let mut rows = Vec::new();
                let mut seen = std::collections::BTreeSet::new();
                for binding in &sessions {
                    if !seen.insert(binding.workspace_id.clone()) {
                        continue;
                    }
                    if let Some(workspace) = store.get_agent_workspace(&binding.workspace_id)?
                        && params
                            .identity_id
                            .as_deref()
                            .is_none_or(|identity| workspace.identity_id == identity)
                        && params
                            .status
                            .as_deref()
                            .is_none_or(|status| workspace.status == status)
                    {
                        rows.push(workspace);
                    }
                }
                rows
            } else {
                store.list_agent_workspaces(
                    project_id.as_deref(),
                    params.identity_id.as_deref(),
                    params.status.as_deref(),
                    params.limit,
                )?
            };
            Ok(AgentWorkspaceListResult {
                workspaces,
                sessions,
            })
        })
        .await?
    }

    pub async fn get_agent_workspace(
        &self,
        params: AgentWorkspaceParams,
    ) -> Result<AgentWorkspaceGetResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let workspace = store
                .get_agent_workspace(&params.workspace_id)?
                .ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_NOT_FOUND,
                        format!("workspace not found: {}", params.workspace_id),
                    )
                })?;
            let sessions = store.list_session_workspaces_for_workspace(&workspace.id)?;
            Ok(AgentWorkspaceGetResult {
                workspace,
                sessions,
            })
        })
        .await?
    }

    /// Create or reuse one active component workspace, and optionally attach it
    /// to one session. This is explicit operator state; it never starts a run.
    pub async fn ensure_agent_workspace(
        &self,
        params: AgentWorkspaceEnsureParams,
    ) -> Result<AgentWorkspaceEnsureResult> {
        let store = self.store.clone();
        let git_lock = self.git_lock.clone();
        let data_dir = self.store.data_dir().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            let session = match params.session_id.as_deref() {
                Some(session_id) => Some(store.get_session(session_id)?.ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_NOT_FOUND,
                        format!("session not found: {session_id}"),
                    )
                })?),
                None => None,
            };
            let project_id = match session.as_ref() {
                Some(session) => session.project_id.clone(),
                None => params.project_id.clone(),
            };
            if project_id.trim().is_empty() {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_INVALID,
                    "project_id is required when no session_id is supplied",
                ));
            }
            if !params.project_id.trim().is_empty() && params.project_id != project_id {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                    "session and project_id belong to different projects",
                ));
            }
            let identity = store
                .get_agent_identity(&params.identity_id)?
                .ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_NOT_FOUND,
                        format!("agent identity not found: {}", params.identity_id),
                    )
                })?;
            if identity.project_id != project_id {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                    "identity and workspace belong to different projects",
                ));
            }
            let component = params
                .component
                .as_deref()
                .or(identity.component.as_deref())
                .unwrap_or("worker");
            let (component, branch) = agent_workspace::component_branch(&identity.id, component)?;
            let project = store.get_project(&project_id)?.ok_or_else(|| {
                agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_FOUND,
                    format!("project not found: {project_id}"),
                )
            })?;
            let root = PathBuf::from(&project.root);
            // A component branch must be rooted at the session's stored base,
            // not at whatever the user's checkout currently points to. Basing
            // it on HEAD after an unrelated land could introduce history the
            // session branch never contained.
            let base = session
                .as_ref()
                .and_then(|session| session.base_commit.as_deref())
                .map(str::trim)
                .filter(|base| !base.is_empty())
                .map(str::to_string)
                .unwrap_or(crate::worktree::git_output(&root, &["rev-parse", "HEAD"])?);
            let component_head = agent_workspace::ensure_component_branch(&root, &branch, &base)?;
            if !agent_workspace::branches_share_history(&root, &base, &component_head)? {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_BASE_MISMATCH,
                    format!(
                        "component branch {branch} does not share the session base {}; refusing to attach unrelated history",
                        crate::workspace_engine::short_commit(&base)
                    ),
                ));
            }
            let workspace_worktree =
                agent_workspace::component_worktree_path(&data_dir, &project_id, &identity.id);
            let prepared = store.prepare_agent_workspace(
                vp_store::store::workspaces::PrepareWorkspaceRequest {
                    project_id: &project_id,
                    identity_id: &identity.id,
                    component: Some(&component),
                    branch: &branch,
                    base_commit: &component_head,
                    worktree_path: &workspace_worktree.to_string_lossy(),
                    session_id: session.as_ref().map(|session| session.id.as_str()),
                    lease_secs: params.lease_secs,
                    operator: params.operator.as_deref(),
                },
            )?;
            let Some(session) = session.as_ref() else {
                return Ok(AgentWorkspaceEnsureResult {
                    workspace: prepared.workspace,
                    session: None,
                    created: prepared.changed,
                    attached: false,
                });
            };
            let binding_branch = agent_workspace::session_branch(&session.id, &identity.id)?;
            let binding_path = agent_workspace::session_worktree_path(
                &data_dir,
                &project_id,
                &session.id,
                &identity.id,
            );
            let existed_before = binding_path.join(".git").is_file();
            if existed_before {
                // Reattaching must not merge a worktree built from an
                // unrelated base. Both directions are accepted: the component
                // may have advanced past the session, or the session branch
                // may contain the component head already.
                let existing_head = crate::worktree::head_commit(&binding_path)?;
                if !agent_workspace::branches_share_history(
                    &root,
                    &component_head,
                    &existing_head,
                )? {
                    return Err(agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_BASE_MISMATCH,
                        format!(
                            "existing session worktree {} does not share history with component head {}",
                            crate::workspace_engine::short_commit(&existing_head),
                            crate::workspace_engine::short_commit(&component_head)
                        ),
                    ));
                }
            }
            let attach = agent_workspace::ensure_worktree(
                &root,
                &binding_path,
                &binding_branch,
                &component_head,
            );
            let attach = match attach {
                Ok(info) => info,
                Err(error) => {
                    let _ = store.release_session_worker_workspace(
                        &session.id,
                        Some(&identity.id),
                        "ensure_failed",
                        None,
                    );
                    return Err(error);
                }
            };
            let attached = store.attach_session_workspace(
                vp_store::store::workspaces::AttachWorkspaceRequest {
                    workspace_id: &prepared.workspace.id,
                    session_id: &session.id,
                    branch: &binding_branch,
                    worktree_path: &attach.path.to_string_lossy(),
                    base_commit: &attach.base_commit,
                    lease_secs: params.lease_secs,
                    operator: params.operator.as_deref(),
                },
            );
            match attached {
                Ok(attached) => Ok(AgentWorkspaceEnsureResult {
                    workspace: attached.workspace,
                    session: attached.session,
                    created: prepared.changed,
                    attached: true,
                }),
                Err(error) => {
                    if !existed_before {
                        let _ = crate::worktree::remove_worktree(&root, &binding_path);
                    }
                    let _ = store.release_session_worker_workspace(
                        &session.id,
                        Some(&identity.id),
                        "attach_failed",
                        None,
                    );
                    Err(error)
                }
            }
        })
        .await?
    }

    pub async fn session_workspace(
        &self,
        params: SessionWorkspaceParams,
    ) -> Result<SessionWorkspaceResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let bindings = store.list_session_worker_workspaces(&params.session_id)?;
            Ok(SessionWorkspaceResult {
                session_id: params.session_id,
                bindings,
            })
        })
        .await?
    }

    /// Resolve and attach the deterministic Worker workspace for one run.
    /// Legacy/no-assignment sessions return `None` and keep the task-worktree
    /// flow exactly. A live component lease held elsewhere is also a fallback so
    /// a parallel plan run does not fail or mutate a busy component.
    pub async fn attach_worker_workspace_for_run(
        &self,
        session_id: &str,
        title: Option<&str>,
        department: Option<&str>,
    ) -> Result<Option<WorkerWorkspaceBinding>> {
        let Some(identity) = self
            .prompt_identity_for_run(session_id, "worker", title, department)
            .await?
        else {
            return Ok(None);
        };
        let result = self
            .ensure_agent_workspace(AgentWorkspaceEnsureParams {
                project_id: identity.project_id.clone(),
                identity_id: identity.id.clone(),
                component: identity.component.clone(),
                session_id: Some(session_id.to_string()),
                lease_secs: Some(3600),
                operator: Some("engine:worker-run".to_string()),
            })
            .await;
        match result {
            Ok(result) => {
                let Some(session) = result.session else {
                    return Ok(None);
                };
                Ok(Some(WorkerWorkspaceBinding {
                    workspace: result.workspace,
                    session,
                }))
            }
            Err(error) => {
                if let Some(typed) = error.downcast_ref::<vp_protocol::types::AgentWorkspaceError>()
                    && matches!(
                        typed.code,
                        agent_workspace_codes::WORKSPACE_BUSY
                            | agent_workspace_codes::WORKSPACE_POLICY_DISABLED
                            | agent_workspace_codes::WORKSPACE_ASSIGNMENT_REQUIRED
                    )
                {
                    tracing::warn!(
                        session_id,
                        code = typed.code,
                        "Worker workspace unavailable; using the legacy task worktree"
                    );
                    // Make the fallback observable. A silent mode switch means
                    // later Labour merges and S6 attribution no longer match
                    // the durable component workspace.
                    let _ = self
                        .append_event(
                            session_id,
                            vp_protocol::types::AGENT_WORKSPACE_FALLBACK_EVENT,
                            serde_json::json!({
                                "identity_id": identity.id,
                                "code": typed.code,
                                "message": typed.message.chars().take(500).collect::<String>(),
                                "mode": "legacy_task_worktree",
                            }),
                        )
                        .await;
                    return Ok(None);
                }
                Err(error)
            }
        }
    }

    /// Merge one Labour commit into its Worker session workspace. The Git
    /// outcome is persisted as `agent.workspace.labour_merged` whether it
    /// succeeded or was rejected, and a failure never leaves partial Git state.
    #[allow(clippy::too_many_arguments)]
    pub async fn merge_labour_commit(
        &self,
        workspace_id: &str,
        session_id: &str,
        agent_id: &str,
        task_id: Option<&str>,
        commit: &str,
        write_scope: &[String],
    ) -> Result<agent_workspace::LabourMergeOutcome> {
        let store = self.store.clone();
        let git_lock = self.git_lock.clone();
        let workspace_id = workspace_id.to_string();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        let task_id = task_id.map(str::to_string);
        let commit = commit.to_string();
        let write_scope = write_scope.to_vec();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            let workspace = store.get_agent_workspace(&workspace_id)?.ok_or_else(|| {
                agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_FOUND,
                    format!("workspace not found: {workspace_id}"),
                )
            })?;
            agent_workspace::ensure_local_active(&workspace)?;
            let binding = store
                .get_session_worker_workspace(&session_id, &workspace.identity_id)?
                .ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_ASSIGNMENT_REQUIRED,
                        "the session has no Worker workspace binding",
                    )
                })?;
            if binding.workspace_id != workspace.id || binding.status != "active" {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_ACTIVE,
                    "the Worker workspace binding is not active",
                ));
            }
            let worktree_path = PathBuf::from(&binding.worktree_path);
            if !worktree_path.join(".git").is_file() {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
                    format!(
                        "Worker workspace worktree is missing: {}",
                        worktree_path.display()
                    ),
                ));
            }
            let message = format!(
                "merge labour {agent_id}{}",
                task_id
                    .as_deref()
                    .map(|task| format!(" task {task}"))
                    .unwrap_or_default()
            );
            let outcome = agent_workspace::merge_labour_commit(
                &worktree_path,
                &commit,
                &message,
                &write_scope,
            )?;
            store.record_workspace_labour_merge(
                &workspace_id,
                &session_id,
                &agent_id,
                task_id.as_deref(),
                &commit,
                &outcome.pre_merge,
                &outcome.post_merge,
                outcome.merged,
                &outcome.paths,
                &outcome.detail,
            )?;
            Ok(outcome)
        })
        .await?
    }

    /// Explicitly land accepted Worker commits onto the component branch. This
    /// is operator-only, requires `confirm = true`, and never auto-runs. The
    /// component branch is rolled back on conflict or a failed diff check.
    pub async fn land_agent_workspace(
        &self,
        params: AgentWorkspaceLandParams,
    ) -> Result<AgentWorkspaceLandResult> {
        if !params.confirm {
            return Err(agent_workspace::workspace_error(
                agent_workspace_codes::WORKSPACE_CONFIRMATION_REQUIRED,
                "workspace.land requires explicit confirm=true",
            ));
        }
        let store = self.store.clone();
        let git_lock = self.git_lock.clone();
        let data_dir = self.store.data_dir().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            let workspace = store
                .get_agent_workspace(&params.workspace_id)?
                .ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_NOT_FOUND,
                        format!("workspace not found: {}", params.workspace_id),
                    )
                })?;
            agent_workspace::ensure_local_active(&workspace)?;
            let session_id = params
                .session_id
                .clone()
                .or_else(|| workspace.last_session_id.clone())
                .ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_INVALID,
                        "workspace has no session to land",
                    )
                })?;
            let session = store.get_session(&session_id)?.ok_or_else(|| {
                agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_FOUND,
                    format!("session not found: {session_id}"),
                )
            })?;
            crate::ensure_session_quiescent(&session, "land a Worker workspace")?;
            if !agent_workspace::session_is_quiescent(&session.status) {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_QUIESCENT,
                    format!("session is {} and cannot land a workspace", session.status),
                ));
            }
            if workspace.lease_active
                && workspace.last_session_id.as_deref() != Some(session_id.as_str())
            {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_BUSY,
                    "workspace has a live lease owned by another session",
                ));
            }
            let binding = store
                .get_session_worker_workspace(&session_id, &workspace.identity_id)?
                .ok_or_else(|| {
                    agent_workspace::workspace_error(
                        agent_workspace_codes::WORKSPACE_ASSIGNMENT_REQUIRED,
                        "the session has no Worker workspace binding to land",
                    )
                })?;
            if binding.workspace_id != workspace.id {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_CONFLICT,
                    "session binding references a different workspace",
                ));
            }
            let project = store.get_project(&workspace.project_id)?.ok_or_else(|| {
                agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_FOUND,
                    format!("project not found: {}", workspace.project_id),
                )
            })?;
            let session_branch = session.branch.clone().ok_or_else(|| {
                agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_INVALID,
                    "session has no branch",
                )
            })?;
            let root = PathBuf::from(&project.root);
            if !crate::worktree::branch_exists(&root, &workspace.branch)? {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
                    format!("component branch is missing: {}", workspace.branch),
                ));
            }
            if !crate::worktree::branch_exists(&root, &binding.branch)? {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
                    format!("session workspace branch is missing: {}", binding.branch),
                ));
            }
            let workspace_commits =
                agent_workspace::commits_between(&root, &workspace.branch, &binding.branch)?;
            let mut accepted_commits = Vec::new();
            let mut unaccepted = Vec::new();
            for commit in &workspace_commits {
                if crate::worktree::is_ancestor(&root, commit, &session_branch)? {
                    accepted_commits.push(commit.clone());
                } else {
                    unaccepted.push(commit.clone());
                }
            }
            if !workspace_commits.is_empty() && accepted_commits.is_empty() {
                return Err(agent_workspace::workspace_error(
                    agent_workspace_codes::WORKSPACE_NOT_ACCEPTED,
                    format!(
                        "the session branch has not accepted any of {} workspace commit(s): {}",
                        unaccepted.len(),
                        unaccepted
                            .iter()
                            .map(|commit| crate::workspace_engine::short_commit(commit))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            let component_head =
                crate::worktree::git_output(&root, &["rev-parse", &workspace.branch])?;
            let component_worktree = agent_workspace::ensure_component_worktree(
                &root,
                &data_dir,
                &workspace.project_id,
                &workspace.identity_id,
                &workspace.branch,
                &component_head,
            )?;
            let message = format!("land workspace {} ({})", workspace.id, workspace.component);
            let outcome = agent_workspace::land_component_branch(
                &component_worktree.path,
                &workspace.branch,
                &binding.branch,
                &accepted_commits,
                &message,
            )?;
            let workspace = if outcome.already_up_to_date {
                store
                    .get_agent_workspace(&workspace.id)?
                    .ok_or_else(|| anyhow!("workspace disappeared after a no-op land"))?
            } else {
                store.record_workspace_landed(
                    &workspace.id,
                    &session_id,
                    &outcome.previous_component_commit,
                    &outcome.landed_commit,
                    &outcome.commits,
                    params.operator.as_deref(),
                )?;
                store
                    .get_agent_workspace(&workspace.id)?
                    .ok_or_else(|| anyhow!("workspace disappeared after land"))?
            };
            Ok(AgentWorkspaceLandResult {
                workspace,
                session_id,
                previous_component_commit: outcome.previous_component_commit,
                landed_commit: outcome.landed_commit,
                already_up_to_date: outcome.already_up_to_date,
                commits: outcome.commits,
                skipped_commits: outcome.skipped_commits,
            })
        })
        .await?
    }

    pub async fn archive_agent_workspace(
        &self,
        params: AgentWorkspaceArchiveParams,
        retire: bool,
    ) -> Result<AgentWorkspaceResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mutation = store.archive_agent_workspace(
                &params.workspace_id,
                retire,
                params.operator.as_deref(),
                params.reason.as_deref(),
            )?;
            Ok(AgentWorkspaceResult {
                workspace: mutation.workspace,
                changed: mutation.changed,
            })
        })
        .await?
    }

    pub async fn retire_agent_workspace(
        &self,
        params: AgentWorkspaceRetireParams,
    ) -> Result<AgentWorkspaceResult> {
        self.archive_agent_workspace(
            AgentWorkspaceArchiveParams {
                workspace_id: params.workspace_id,
                operator: params.operator,
                reason: params.reason,
            },
            true,
        )
        .await
    }

    /// Read-only workspace replay, or explicit projection repair. Repair never
    /// appends events and clears operational leases; only run it while
    /// quiescent.
    pub async fn replay_agent_workspaces(
        &self,
        params: AgentWorkspaceReplayParams,
    ) -> Result<AgentWorkspaceReplayReport> {
        let store = self.store.clone();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            let events = store.list_agent_workspace_events_for_project(&project_id, None)?;
            let expected = agent_workspace::project_from_events(&events)?;
            let mut actual_workspaces = store.list_agent_workspaces_for_replay(&project_id)?;
            let mut actual_sessions = store.list_session_workspaces_for_replay(&project_id)?;
            let mut mismatches = agent_workspace::compare_workspace_projection(
                &expected,
                &actual_workspaces,
                &actual_sessions,
            );
            let mut repaired = false;
            if repair && !mismatches.is_empty() {
                store.replace_agent_workspace_projection(
                    &project_id,
                    &expected.workspaces,
                    &expected.sessions,
                )?;
                actual_workspaces = store.list_agent_workspaces_for_replay(&project_id)?;
                actual_sessions = store.list_session_workspaces_for_replay(&project_id)?;
                mismatches = agent_workspace::compare_workspace_projection(
                    &expected,
                    &actual_workspaces,
                    &actual_sessions,
                );
                repaired = true;
            }
            Ok(agent_workspace::replay_report(
                &project_id,
                expected.workspaces.len(),
                expected.sessions.len(),
                events.len(),
                repaired,
                mismatches,
            ))
        })
        .await?
    }

    /// Release every active workspace lease owned by a session. Called on
    /// session end, startup/recovery, and the periodic coordination sweep.
    pub async fn release_session_workspaces(&self, session_id: &str, reason: &str) -> Result<u64> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let reason = reason.to_string();
        let events = tokio::task::spawn_blocking(move || {
            store.release_session_worker_workspace(&session_id, None, &reason, None)
        })
        .await??;
        Ok(events.len() as u64)
    }

    pub async fn sweep_agent_workspaces(&self, limit: Option<u32>) -> Result<u64> {
        let store = self.store.clone();
        let rows =
            tokio::task::spawn_blocking(move || store.reconcile_agent_workspace_leases(limit))
                .await??;
        Ok(rows.len() as u64)
    }
}

/// Short commit label for bounded error messages.
pub fn short_commit(commit: &str) -> String {
    commit.chars().take(12).collect()
}

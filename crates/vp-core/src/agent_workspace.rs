//! Worker component workspaces and two-level integration (S4).
//!
//! This module owns the deterministic Git side of a project-scoped Worker
//! workspace and the pure event replay that reconstructs workspace/binding
//! projections. It deliberately contains no provider I/O, no model routing, and
//! no approval bypass:
//!
//! ```text
//! project repository
//!   └── component branch (per Worker identity, long-lived)
//!         └── session worker worktree (one branch per session attachment)
//!               └── Labour task worktree (one branch per Labour run)
//! ```
//!
//! Labour commits are merged into the session Worker worktree with an explicit
//! `--no-ff` merge, a path-scope check, `git diff --check`, and a hard rollback
//! to the pre-merge commit on any failure. That is an internal Worker review
//! step. The existing Verifier and deterministic Integrator still gate every
//! Worker commit that reaches the session branch, and only an explicit
//! `workspace.land` merges accepted Worker commits into the component branch.
//!
//! All Git operations run under the caller's project/Git lock. A workspace
//! lease is same-host SQLite coordination, not a distributed lock or a security
//! credential.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use vp_protocol::types::{
    AGENT_WORKSPACE_EVENT_KINDS, AGENT_WORKSPACE_MAX_PATHS, AGENT_WORKSPACE_SESSION_BRANCH_PREFIX,
    AGENT_WORKSPACE_STATUSES, AgentWorkspace, AgentWorkspaceError, AgentWorkspaceEvent,
    AgentWorkspaceReplayReport, SessionWorkerWorkspace, agent_workspace_codes,
    component_branch_name, normalize_workspace_component, session_workspace_branch_name,
    workspace_ref_slug,
};

use crate::worktree;

/// Typed workspace failure helper shared by the engine and store boundaries.
pub fn workspace_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentWorkspaceError::new(code, message).into()
}

/// Bounded outcome of one internal Labour→Worker merge. A conflict is a normal
/// typed outcome, not a process error; infrastructure failures remain errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabourMergeOutcome {
    pub merged: bool,
    pub pre_merge: String,
    pub post_merge: String,
    pub paths: Vec<String>,
    pub conflicts: Vec<String>,
    pub detail: String,
}

/// Bounded outcome of one explicit component-branch land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandOutcome {
    pub already_up_to_date: bool,
    pub previous_component_commit: String,
    pub landed_commit: String,
    pub commits: Vec<String>,
    /// Commits deliberately excluded because the session branch never
    /// accepted them. They remain on the workspace branch as evidence.
    pub skipped_commits: Vec<String>,
}

/// Validate a Worker identity id before it becomes a directory or ref suffix.
pub fn validate_identity_id(identity_id: &str) -> Result<()> {
    let value = identity_id.trim();
    if value.is_empty() || value.len() > 128 {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_INVALID,
            "identity id must be 1-128 characters",
        ));
    }
    if value != identity_id
        || value == "."
        || value == ".."
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_INVALID,
            "identity id must be a single safe path component",
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IDENTITY_INVALID,
            "identity id may only contain ASCII letters, digits, '-', '_', and '.'",
        ));
    }
    Ok(())
}

/// Validate a session id before it becomes a directory component.
pub fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty() || session_id.len() > 200 {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            "session id must be 1-200 characters",
        ));
    }
    if session_id == "."
        || session_id == ".."
        || session_id.contains("..")
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.chars().any(char::is_control)
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            "session id must be a single safe path component",
        ));
    }
    Ok(())
}

/// Validate a deterministic component branch. Only the S4 namespace is
/// accepted; an arbitrary operator branch can never be created or landed.
pub fn validate_component_branch(branch: &str) -> Result<()> {
    if !branch.starts_with(vp_protocol::types::AGENT_WORKSPACE_COMPONENT_BRANCH_PREFIX) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
            "component branch must use the verlinpark/components/ namespace",
        ));
    }
    validate_branch_shape(branch)
}

/// Validate a deterministic session Worker branch.
pub fn validate_session_branch(branch: &str) -> Result<()> {
    if !branch.starts_with(AGENT_WORKSPACE_SESSION_BRANCH_PREFIX) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
            "session workspace branch must use the verlinpark/sessions/ namespace",
        ));
    }
    validate_branch_shape(branch)
}

fn validate_branch_shape(branch: &str) -> Result<()> {
    if branch.len() > 240
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.contains("..")
        || branch.contains("//")
        || branch.ends_with(".lock")
        || branch.chars().any(|ch| {
            ch.is_control() || ch.is_whitespace() || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[')
        })
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_BRANCH_INVALID,
            format!("invalid workspace branch: {branch}"),
        ));
    }
    Ok(())
}

/// Deterministic component branch for one identity/component pair.
pub fn component_branch(identity_id: &str, component: &str) -> Result<(String, String)> {
    validate_identity_id(identity_id)?;
    let component = normalize_workspace_component(component)?;
    let branch = component_branch_name(identity_id, &component);
    validate_component_branch(&branch)?;
    Ok((component, branch))
}

/// Deterministic session Worker branch for one session/identity pair.
pub fn session_branch(session_id: &str, identity_id: &str) -> Result<String> {
    validate_session_id(session_id)?;
    validate_identity_id(identity_id)?;
    let branch = session_workspace_branch_name(session_id, identity_id);
    validate_session_branch(&branch)?;
    Ok(branch)
}

/// Persistent checkout for the component branch. It is created lazily by
/// `land`; a session only needs its own worktree.
pub fn component_worktree_path(data_dir: &Path, project_id: &str, identity_id: &str) -> PathBuf {
    data_dir
        .join("workspaces")
        .join(workspace_ref_slug(project_id))
        .join("components")
        .join(workspace_ref_slug(identity_id))
}

/// Per-session Worker worktree path. It is deliberately outside the existing
/// `worktrees/` tree so a task id can never collide with it.
pub fn session_worktree_path(
    data_dir: &Path,
    project_id: &str,
    session_id: &str,
    identity_id: &str,
) -> PathBuf {
    data_dir
        .join("workspaces")
        .join(workspace_ref_slug(project_id))
        .join("sessions")
        .join(workspace_ref_slug(session_id))
        .join(workspace_ref_slug(identity_id))
}

/// Ensure the component branch exists at `base_commit` and return its current
/// head. A branch that already exists is never reset.
pub fn ensure_component_branch(
    project_root: &Path,
    branch: &str,
    base_commit: &str,
) -> Result<String> {
    validate_component_branch(branch)?;
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    if worktree::branch_exists(&root, branch)? {
        return worktree::git_output(&root, &["rev-parse", branch]);
    }
    worktree::git_output(
        &root,
        &[
            "rev-parse",
            "--verify",
            &format!("{base_commit}^{{commit}}"),
        ],
    )
    .with_context(|| format!("resolving workspace base commit {base_commit}"))?;
    worktree::git_output(&root, &["branch", branch, base_commit])?;
    worktree::git_output(&root, &["rev-parse", branch])
}

/// Create or reattach a worktree checked out on an existing branch. The caller
/// must have validated the branch namespace and must hold the Git lock.
pub fn ensure_worktree(
    project_root: &Path,
    worktree_path: &Path,
    branch: &str,
    base_commit: &str,
) -> Result<worktree::WorktreeInfo> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    worktree::git_output(&root, &["worktree", "prune"])?;
    if worktree_path.join(".git").is_file() {
        let current = worktree::current_branch(worktree_path).unwrap_or_default();
        if current.trim() != branch {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
                format!(
                    "existing workspace worktree {} is on '{}', expected '{}'",
                    worktree_path.display(),
                    current.trim(),
                    branch
                ),
            ));
        }
        let head = worktree::head_commit(worktree_path)?;
        return Ok(worktree::WorktreeInfo {
            path: worktree_path.to_path_buf(),
            branch: branch.to_string(),
            base_commit: head,
        });
    }
    if worktree_path.exists() {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
            format!(
                "workspace path exists but is not a Git worktree: {}",
                worktree_path.display()
            ),
        ));
    }
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree directory {}", parent.display()))?;
    }
    if !worktree::branch_exists(&root, branch)? {
        worktree::git_output(
            &root,
            &[
                "rev-parse",
                "--verify",
                &format!("{base_commit}^{{commit}}"),
            ],
        )
        .with_context(|| format!("resolving worktree base commit {base_commit}"))?;
        worktree::git_output(&root, &["branch", branch, base_commit])?;
    }
    worktree::git_output(
        &root,
        &[
            "worktree",
            "add",
            worktree_path
                .to_str()
                .ok_or_else(|| anyhow!("workspace worktree path is not valid UTF-8"))?,
            branch,
        ],
    )?;
    let head = worktree::head_commit(worktree_path)?;
    Ok(worktree::WorktreeInfo {
        path: worktree_path.to_path_buf(),
        branch: branch.to_string(),
        base_commit: head,
    })
}

/// Ensure the long-lived component checkout exists. Used only by an explicit
/// land so ordinary sessions do not pay for a second full checkout.
pub fn ensure_component_worktree(
    project_root: &Path,
    data_dir: &Path,
    project_id: &str,
    identity_id: &str,
    branch: &str,
    base_commit: &str,
) -> Result<worktree::WorktreeInfo> {
    validate_component_branch(branch)?;
    let path = component_worktree_path(data_dir, project_id, identity_id);
    ensure_worktree(project_root, &path, branch, base_commit)
}

/// Commits on `branch` that are not yet reachable from `base`, newest last.
/// The output is bounded so a pathological branch cannot flood an event.
pub fn commits_between(worktree_root: &Path, base: &str, branch: &str) -> Result<Vec<String>> {
    let output = worktree::git_output(
        worktree_root,
        &[
            "rev-list",
            "--reverse",
            "--max-count=1000",
            &format!("{base}..{branch}"),
        ],
    )?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// True when two commits share history in either direction. Unrelated roots
/// must never be merged into a session component branch.
pub fn branches_share_history(project_root: &Path, left: &str, right: &str) -> Result<bool> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    Ok(worktree::is_ancestor(&root, left, right)? || worktree::is_ancestor(&root, right, left)?)
}

/// All commits in the workspace branch must be reachable from the session
/// branch before an explicit land. Integrator merges exact verified commits, so
/// a subset is expected; an unaccepted commit fails closed.
pub fn unaccepted_commits(
    project_root: &Path,
    component_branch: &str,
    workspace_branch: &str,
    session_branch: &str,
) -> Result<Vec<String>> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    let mut unaccepted = Vec::new();
    for commit in commits_between(&root, component_branch, workspace_branch)? {
        if !worktree::is_ancestor(&root, &commit, session_branch)? {
            unaccepted.push(commit);
            if unaccepted.len() >= AGENT_WORKSPACE_MAX_PATHS {
                break;
            }
        }
    }
    Ok(unaccepted)
}

/// Merge one Labour commit into the session Worker worktree.
///
/// Preconditions: the Worker worktree is clean, the commit exists, and every
/// path changed between the current HEAD and the commit is inside the Labour
/// write scope (when one is declared). Any conflict, whitespace error, or Git
/// failure aborts and resets the Worker worktree to its exact pre-merge commit,
/// so a failed internal merge never leaves partial work behind.
pub fn merge_labour_commit(
    worker_worktree: &Path,
    commit: &str,
    message: &str,
    write_scope: &[String],
) -> Result<LabourMergeOutcome> {
    let commit = commit.trim();
    if commit.len() < 7 || commit.len() > 64 || !commit.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!("invalid Labour commit: {commit}"),
        ));
    }
    let pre_merge = worktree::head_commit(worker_worktree)?;
    if !worktree::is_clean(worker_worktree)? {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_QUIESCENT,
            "Worker workspace has uncommitted tracked changes; refusing to merge Labour work",
        ));
    }
    if !worktree::commit_exists(worker_worktree, commit) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!("Labour commit does not exist: {commit}"),
        ));
    }
    let diff_paths = worktree::git_output(
        worker_worktree,
        &["diff", "--name-only", &pre_merge, commit],
    )?;
    let mut paths = diff_paths
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let outside = paths
        .iter()
        .filter(|path| !crate::agent::path_in_scope(path, write_scope))
        .cloned()
        .collect::<Vec<_>>();
    if !outside.is_empty() {
        return Ok(LabourMergeOutcome {
            merged: false,
            pre_merge: pre_merge.clone(),
            post_merge: pre_merge,
            paths,
            conflicts: Vec::new(),
            detail: format!(
                "write scope violation: {}",
                truncate(&outside.join(", "), 600)
            ),
        });
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(worker_worktree)
        .args(["merge", "--no-ff", commit, "-m", message])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("merging Labour commit")?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        let conflicts = worktree::conflicted_paths(worker_worktree).unwrap_or_default();
        let _ = worktree::git_output(worker_worktree, &["merge", "--abort"]);
        let reset = worktree::reset_hard(worker_worktree, &pre_merge);
        let post_merge = worktree::head_commit(worker_worktree).unwrap_or_default();
        let detail = match reset {
            Ok(()) if post_merge == pre_merge => {
                format!("Labour merge failed and was rolled back: {detail}")
            }
            Ok(()) => format!(
                "Labour merge failed and rollback left HEAD at {post_merge} (expected {pre_merge}): {detail}"
            ),
            Err(error) => format!(
                "Labour merge failed; rollback to {pre_merge} failed: {error}; git said: {detail}"
            ),
        };
        return Ok(LabourMergeOutcome {
            merged: false,
            pre_merge,
            post_merge,
            paths,
            conflicts,
            detail: truncate(&detail, 2_000),
        });
    }
    let post_merge = worktree::head_commit(worker_worktree)?;
    if let Err(error) = worktree::check_merged_diff(worker_worktree, &pre_merge) {
        let _ = worktree::reset_hard(worker_worktree, &pre_merge);
        let post = worktree::head_commit(worker_worktree).unwrap_or_default();
        return Ok(LabourMergeOutcome {
            merged: false,
            pre_merge: pre_merge.clone(),
            post_merge: post,
            paths,
            conflicts: Vec::new(),
            detail: truncate(
                &format!("internal integration check failed and was rolled back: {error}"),
                2_000,
            ),
        });
    }
    let merged_paths = worktree::git_output(
        worker_worktree,
        &["diff", "--name-only", &pre_merge, &post_merge],
    )?
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect::<Vec<_>>();
    Ok(LabourMergeOutcome {
        merged: true,
        pre_merge,
        post_merge,
        paths: merged_paths,
        conflicts: Vec::new(),
        detail: "Labour commit integrated into the Worker workspace".to_string(),
    })
}

/// Merge a session Worker branch into the long-lived component branch.
///
/// The caller must have verified that the session is quiescent and that every
/// workspace commit is accepted by the session branch. The component checkout
/// is clean or the land fails closed; a conflict or failed diff check resets the
/// component branch to its exact pre-land commit.
pub fn land_component_branch(
    component_worktree: &Path,
    component_branch: &str,
    workspace_branch: &str,
    accepted_commits: &[String],
    message: &str,
) -> Result<LandOutcome> {
    validate_component_branch(component_branch)?;
    validate_session_branch(workspace_branch)?;
    let current = worktree::current_branch(component_worktree)?;
    if current.trim() != component_branch {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_WORKTREE_INVALID,
            format!(
                "component worktree is on '{}', expected '{}'",
                current.trim(),
                component_branch
            ),
        ));
    }
    if !worktree::is_clean(component_worktree)? {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_QUIESCENT,
            "component workspace has uncommitted tracked changes; refusing to land",
        ));
    }
    let previous = worktree::head_commit(component_worktree)?;
    let commits = commits_between(component_worktree, &previous, workspace_branch)?;
    if commits.is_empty() {
        return Ok(LandOutcome {
            already_up_to_date: true,
            previous_component_commit: previous.clone(),
            landed_commit: previous,
            commits: Vec::new(),
            skipped_commits: Vec::new(),
        });
    }
    let accepted = accepted_commits
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let skipped_commits = commits
        .iter()
        .filter(|commit| !accepted.contains(*commit))
        .cloned()
        .collect::<Vec<_>>();
    if accepted_commits.is_empty() {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_ACCEPTED,
            "the session branch has not accepted any workspace commit",
        ));
    }
    if skipped_commits.is_empty() {
        // Every workspace commit is accepted: keep the deterministic merge
        // shape so a normal land remains one --no-ff commit.
        let output = Command::new("git")
            .arg("-C")
            .arg(component_worktree)
            .args(["merge", "--no-ff", workspace_branch, "-m", message])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("landing workspace branch")?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            let conflicts = worktree::conflicted_paths(component_worktree).unwrap_or_default();
            rollback_component_branch(component_worktree, &previous, "workspace land conflicted")?;
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_LAND_CONFLICT,
                format!(
                    "workspace land conflicted and was rolled back to {previous}: {}; conflicts: {}",
                    truncate(&detail, 1_200),
                    truncate(&conflicts.join(", "), 400)
                ),
            ));
        }
        let landed = worktree::head_commit(component_worktree)?;
        if let Err(error) = worktree::check_merged_diff(component_worktree, &previous) {
            rollback_component_branch(
                component_worktree,
                &previous,
                "workspace land check failed",
            )?;
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_LAND_CHECK_FAILED,
                format!("workspace land check failed and was rolled back: {error}"),
            ));
        }
        return Ok(LandOutcome {
            already_up_to_date: false,
            previous_component_commit: previous,
            landed_commit: landed,
            commits,
            skipped_commits: Vec::new(),
        });
    }
    // A rejected commit must not permanently block land. Apply only the
    // accepted commits (those already reachable from the session branch) in
    // history order, and report the skipped ones explicitly.
    let ordered = commits
        .iter()
        .filter(|commit| accepted.contains(*commit))
        .cloned()
        .collect::<Vec<_>>();
    for commit in &ordered {
        let output = Command::new("git")
            .arg("-C")
            .arg(component_worktree)
            .args(["cherry-pick", "-x", commit])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .context("landing accepted workspace commit")?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            let conflicts = worktree::conflicted_paths(component_worktree).unwrap_or_default();
            let _ = worktree::git_output(component_worktree, &["cherry-pick", "--abort"]);
            rollback_component_branch(
                component_worktree,
                &previous,
                "accepted-subset land conflicted",
            )?;
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_LAND_CONFLICT,
                format!(
                    "accepted-subset land conflicted at {} and was rolled back to {previous}: {}; conflicts: {}",
                    truncate(commit, 40),
                    truncate(&detail, 1_200),
                    truncate(&conflicts.join(", "), 400)
                ),
            ));
        }
    }
    let landed = worktree::head_commit(component_worktree)?;
    if let Err(error) = worktree::check_merged_diff(component_worktree, &previous) {
        rollback_component_branch(
            component_worktree,
            &previous,
            "accepted-subset land check failed",
        )?;
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_LAND_CHECK_FAILED,
            format!("accepted-subset land check failed and was rolled back: {error}"),
        ));
    }
    Ok(LandOutcome {
        already_up_to_date: false,
        previous_component_commit: previous,
        landed_commit: landed,
        commits: ordered,
        skipped_commits,
    })
}

/// Reset the component branch to its exact pre-land commit and verify HEAD.
/// A rollback that cannot prove the old HEAD is returned as a distinct typed
/// failure so an operator never assumes the branch is clean.
fn rollback_component_branch(
    component_worktree: &Path,
    previous: &str,
    context: &str,
) -> Result<()> {
    let _ = worktree::git_output(component_worktree, &["cherry-pick", "--abort"]);
    let _ = worktree::git_output(component_worktree, &["merge", "--abort"]);
    let reset = worktree::reset_hard(component_worktree, previous);
    let head = worktree::head_commit(component_worktree).unwrap_or_default();
    if reset.is_ok() && head == previous {
        return Ok(());
    }
    Err(workspace_error(
        agent_workspace_codes::WORKSPACE_LAND_ROLLBACK_FAILED,
        format!("{context}; rollback verification failed: HEAD is {head}, expected {previous}"),
    ))
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

/// Bounded event-derived projection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceProjection {
    pub workspaces: Vec<AgentWorkspace>,
    pub sessions: Vec<SessionWorkerWorkspace>,
}

fn validate_workspace_snapshot(workspace: &AgentWorkspace) -> Result<()> {
    if workspace.id.is_empty()
        || workspace.project_id.is_empty()
        || workspace.identity_id.is_empty()
        || workspace.component.is_empty()
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID_EVENT,
            "workspace snapshot is missing an id, project, identity, or component",
        ));
    }
    if !AGENT_WORKSPACE_STATUSES.contains(&workspace.status.as_str()) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID_EVENT,
            format!("workspace {} has an invalid status", workspace.id),
        ));
    }
    validate_identity_id(&workspace.identity_id)?;
    normalize_workspace_component(&workspace.component)?;
    validate_component_branch(&workspace.branch)?;
    if !matches!(workspace.origin.as_str(), "local" | "imported") {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID_EVENT,
            format!("workspace {} has an invalid origin", workspace.id),
        ));
    }
    Ok(())
}

fn validate_session_snapshot(session: &SessionWorkerWorkspace) -> Result<()> {
    if session.id.is_empty()
        || session.workspace_id.is_empty()
        || session.session_id.is_empty()
        || session.identity_id.is_empty()
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID_EVENT,
            "session workspace snapshot is missing an id, workspace, session, or identity",
        ));
    }
    if !vp_protocol::types::AGENT_SESSION_WORKSPACE_STATUSES.contains(&session.status.as_str()) {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID_EVENT,
            format!("session workspace {} has an invalid status", session.id),
        ));
    }
    validate_session_id(&session.session_id)?;
    validate_identity_id(&session.identity_id)?;
    validate_session_branch(&session.branch)?;
    Ok(())
}

fn workspace_snapshot_from_payload(payload: &serde_json::Value) -> Result<AgentWorkspace> {
    let workspace: AgentWorkspace =
        serde_json::from_value(payload.get("workspace").cloned().ok_or_else(|| {
            workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                "workspace event has no workspace snapshot",
            )
        })?)?;
    validate_workspace_snapshot(&workspace)?;
    Ok(workspace)
}

fn session_snapshot_from_payload(
    payload: &serde_json::Value,
) -> Result<Option<SessionWorkerWorkspace>> {
    let Some(value) = payload.get("session").cloned() else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let session: SessionWorkerWorkspace = serde_json::from_value(value)?;
    validate_session_snapshot(&session)?;
    Ok(Some(session))
}

/// Fold the append-only workspace stream into projections. Events must already
/// be ordered by `(project_id, seq)`. Duplicate or out-of-order sequences fail
/// closed rather than inventing a row.
pub fn project_from_events(events: &[AgentWorkspaceEvent]) -> Result<WorkspaceProjection> {
    let mut latest_workspaces: BTreeMap<String, (String, u64, AgentWorkspace)> = BTreeMap::new();
    let mut latest_sessions: BTreeMap<String, (String, u64, SessionWorkerWorkspace)> =
        BTreeMap::new();
    for event in events {
        if !AGENT_WORKSPACE_EVENT_KINDS.contains(&event.kind.as_str()) {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                format!("unknown workspace event kind: {}", event.kind),
            ));
        }
        if event.id.is_empty() || event.project_id.is_empty() {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                "workspace event is missing an id or project id",
            ));
        }
        let workspace = workspace_snapshot_from_payload(&event.payload)?;
        if workspace.project_id != event.project_id {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_PROJECT_MISMATCH,
                format!("workspace event {} belongs to another project", event.id),
            ));
        }
        if let Some((_, seq, _)) = latest_workspaces.get(&workspace.id)
            && *seq >= event.seq
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                format!(
                    "workspace event stream for {} is out of order at seq {}",
                    workspace.id, event.seq
                ),
            ));
        }
        latest_workspaces.insert(
            workspace.id.clone(),
            (event.id.clone(), event.seq, workspace.clone()),
        );
        if let Some(session) = session_snapshot_from_payload(&event.payload)? {
            if session.workspace_id != workspace.id {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                    format!(
                        "session workspace {} references a different workspace",
                        session.id
                    ),
                ));
            }
            if let Some((_, seq, _)) = latest_sessions.get(&session.id)
                && *seq >= event.seq
            {
                return Err(workspace_error(
                    agent_workspace_codes::WORKSPACE_INVALID_EVENT,
                    format!(
                        "session workspace event stream for {} is out of order at seq {}",
                        session.id, event.seq
                    ),
                ));
            }
            latest_sessions.insert(session.id.clone(), (event.id.clone(), event.seq, session));
        }
    }
    let workspaces = latest_workspaces
        .into_values()
        .map(|(_, _, workspace)| workspace)
        .collect::<Vec<_>>();
    let sessions = latest_sessions
        .into_values()
        .map(|(_, _, session)| session)
        .collect::<Vec<_>>();
    let mut active_components = BTreeSet::new();
    for workspace in &workspaces {
        if workspace.status == "active"
            && workspace.lease_active
            && !active_components
                .insert((workspace.project_id.clone(), workspace.component.clone()))
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_CONFLICT,
                format!(
                    "replayed projection has two leased workspaces for component {}",
                    workspace.component
                ),
            ));
        }
    }
    let mut active_sessions = BTreeSet::new();
    for session in &sessions {
        if session.status == "active"
            && !active_sessions.insert((session.session_id.clone(), session.identity_id.clone()))
        {
            return Err(workspace_error(
                agent_workspace_codes::WORKSPACE_CONFLICT,
                format!(
                    "replayed projection has two active workspaces for session {} identity {}",
                    session.session_id, session.identity_id
                ),
            ));
        }
    }
    Ok(WorkspaceProjection {
        workspaces,
        sessions,
    })
}

/// Compare event-derived rows with stored rows. The lease token is intentionally
/// excluded because it never leaves the store; `lease_active` is derived from
/// the stored expiry.
pub fn compare_workspace_projection(
    expected: &WorkspaceProjection,
    actual_workspaces: &[AgentWorkspace],
    actual_sessions: &[SessionWorkerWorkspace],
) -> Vec<String> {
    let mut mismatches = Vec::new();
    let expected_by_id = expected
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.as_str(), workspace))
        .collect::<BTreeMap<_, _>>();
    let actual_by_id = actual_workspaces
        .iter()
        .map(|workspace| (workspace.id.as_str(), workspace))
        .collect::<BTreeMap<_, _>>();
    for id in expected_by_id
        .keys()
        .chain(actual_by_id.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (expected_by_id.get(id), actual_by_id.get(id)) {
            (Some(expected), Some(actual)) if comparable_workspace(expected, actual) => {}
            (Some(expected), Some(actual)) => mismatches.push(format!(
                "workspace {id}: projection status {} base {} does not match replay status {} base {}",
                actual.status, actual.base_commit, expected.status, expected.base_commit
            )),
            (Some(_), None) => {
                mismatches.push(format!("workspace {id}: event stream exists without a row"))
            }
            (None, Some(_)) => {
                mismatches.push(format!("workspace {id}: row exists without a workspace event"))
            }
            (None, None) => {}
        }
    }
    let expected_sessions = expected
        .sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect::<BTreeMap<_, _>>();
    let actual_sessions = actual_sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect::<BTreeMap<_, _>>();
    for id in expected_sessions
        .keys()
        .chain(actual_sessions.keys())
        .copied()
        .collect::<BTreeSet<_>>()
    {
        match (expected_sessions.get(id), actual_sessions.get(id)) {
            (Some(expected), Some(actual)) if comparable_session(expected, actual) => {}
            (Some(expected), Some(actual)) => mismatches.push(format!(
                "session workspace {id}: projection status {} does not match replay status {}",
                actual.status, expected.status
            )),
            (Some(_), None) => mismatches.push(format!(
                "session workspace {id}: event stream exists without a row"
            )),
            (None, Some(_)) => mismatches.push(format!(
                "session workspace {id}: row exists without a workspace event"
            )),
            (None, None) => {}
        }
    }
    mismatches
}

fn comparable_workspace(left: &AgentWorkspace, right: &AgentWorkspace) -> bool {
    left.id == right.id
        && left.project_id == right.project_id
        && left.identity_id == right.identity_id
        && left.component == right.component
        && left.branch == right.branch
        && left.base_commit == right.base_commit
        && left.worktree_path == right.worktree_path
        && left.status == right.status
        && left.last_session_id == right.last_session_id
        && left.origin == right.origin
}

fn comparable_session(left: &SessionWorkerWorkspace, right: &SessionWorkerWorkspace) -> bool {
    left.id == right.id
        && left.workspace_id == right.workspace_id
        && left.session_id == right.session_id
        && left.identity_id == right.identity_id
        && left.worktree_path == right.worktree_path
        && left.branch == right.branch
        && left.base_commit == right.base_commit
        && left.status == right.status
}

/// Read-only replay report.
pub fn replay_report(
    project_id: &str,
    workspaces: usize,
    sessions: usize,
    events: usize,
    repaired: bool,
    mismatches: Vec<String>,
) -> AgentWorkspaceReplayReport {
    AgentWorkspaceReplayReport {
        project_id: project_id.to_string(),
        workspaces: workspaces as u64,
        sessions: sessions as u64,
        events: events as u64,
        consistent: mismatches.is_empty(),
        repaired,
        mismatches,
    }
}

/// Bounded status shorthand used by store and engine validation.
pub fn is_active(workspace: &AgentWorkspace) -> bool {
    workspace.status == "active" && workspace.origin == "local"
}

/// True when the workspace row can still be attached, landed, or archived.
pub fn ensure_local_active(workspace: &AgentWorkspace) -> Result<()> {
    if workspace.origin != "local" {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_IMPORTED_READONLY,
            "an imported workspace is read-only evidence",
        ));
    }
    if workspace.status != "active" {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_NOT_ACTIVE,
            format!("workspace {} is {}", workspace.id, workspace.status),
        ));
    }
    Ok(())
}

/// Session statuses that may hold a live workspace lease. Everything else is
/// quiescent and eligible for release/land.
pub fn session_is_quiescent(status: &str) -> bool {
    matches!(
        status,
        "idle" | "failed" | "cancelled" | "interrupted" | "landed"
    )
}

/// Validate a workspace archive/retire reason without storing free-form paths.
pub fn validate_reason(reason: Option<&str>) -> Result<Option<String>> {
    let Some(reason) = reason.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if reason.chars().count() > vp_protocol::types::AGENT_WORKSPACE_MAX_REASON_CHARS
        || reason.chars().any(char::is_control)
    {
        return Err(workspace_error(
            agent_workspace_codes::WORKSPACE_INVALID,
            format!(
                "reason must be 1-{} printable characters",
                vp_protocol::types::AGENT_WORKSPACE_MAX_REASON_CHARS
            ),
        ));
    }
    Ok(Some(reason.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn run(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        run(path, &["init", "-q"]);
        run(path, &["config", "user.email", "workspace@test.local"]);
        run(path, &["config", "user.name", "Workspace Test"]);
        std::fs::write(path.join("README.md"), "workspace\n").unwrap();
        run(path, &["add", "."]);
        run(path, &["commit", "-q", "-m", "initial"]);
    }

    fn commit_file(path: &Path, name: &str, content: &str, message: &str) -> String {
        std::fs::write(path.join(name), content).unwrap();
        run(path, &["add", name]);
        run(path, &["commit", "-q", "-m", message]);
        worktree::head_commit(path).unwrap()
    }

    #[test]
    fn validates_identity_components_branches_and_paths() {
        assert!(validate_identity_id("agt-1").is_ok());
        for invalid in ["", "..", "a/b", "a b", "agt:1"] {
            assert!(validate_identity_id(invalid).is_err(), "{invalid:?}");
        }
        let (component, branch) = component_branch("agt-1", "Auth API").unwrap();
        assert_eq!(component, "Auth API");
        assert!(branch.starts_with("verlinpark/components/"));
        assert!(validate_component_branch(&branch).is_ok());
        assert!(validate_component_branch("main").is_err());
        assert!(validate_component_branch("verlinpark/components/../x").is_err());
        let session = session_branch("ses-1", "agt-1").unwrap();
        assert!(validate_session_branch(&session).is_ok());
        assert!(validate_session_branch(&branch).is_err());
        assert!(session_is_quiescent("idle"));
        assert!(!session_is_quiescent("running"));
        assert!(validate_reason(Some("retired")).is_ok());
        assert!(validate_reason(Some("bad\nreason")).is_err());
    }

    #[test]
    fn merges_a_labour_commit_with_scope_and_diff_checks() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "worker"]);
        run(repo.path(), &["checkout", "-q", "-b", "labour", &base]);
        let commit = commit_file(repo.path(), "feature.txt", "feature\n", "labour work");
        run(repo.path(), &["checkout", "-q", "worker"]);
        let outcome = merge_labour_commit(
            repo.path(),
            &commit,
            "merge labour",
            &["feature.txt".to_string()],
        )
        .unwrap();
        assert!(outcome.merged, "{}", outcome.detail);
        assert_eq!(outcome.conflicts, Vec::<String>::new());
        assert!(outcome.paths.contains(&"feature.txt".to_string()));
        assert_eq!(
            worktree::head_commit(repo.path()).unwrap(),
            outcome.post_merge
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("feature.txt")).unwrap(),
            "feature\n"
        );
    }

    #[test]
    fn rejects_out_of_scope_labour_paths_without_moving_head() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "worker"]);
        let pre = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "labour", &base]);
        let commit = commit_file(repo.path(), "outside.txt", "nope\n", "labour work");
        run(repo.path(), &["checkout", "-q", "worker"]);
        let outcome = merge_labour_commit(
            repo.path(),
            &commit,
            "merge labour",
            &["allowed/".to_string()],
        )
        .unwrap();
        assert!(!outcome.merged);
        assert_eq!(outcome.pre_merge, pre);
        assert_eq!(outcome.post_merge, pre);
        assert!(outcome.detail.contains("write scope violation"));
        assert_eq!(worktree::head_commit(repo.path()).unwrap(), pre);
    }

    #[test]
    fn conflict_aborts_and_rolls_back_to_the_exact_pre_merge_commit() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "worker"]);
        commit_file(repo.path(), "README.md", "worker\n", "worker change");
        let pre = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "labour", &base]);
        let commit = commit_file(repo.path(), "README.md", "labour\n", "labour change");
        run(repo.path(), &["checkout", "-q", "worker"]);
        let outcome = merge_labour_commit(
            repo.path(),
            &commit,
            "merge labour",
            &["README.md".to_string()],
        )
        .unwrap();
        assert!(!outcome.merged);
        assert_eq!(outcome.conflicts, vec!["README.md".to_string()]);
        assert_eq!(worktree::head_commit(repo.path()).unwrap(), pre);
        assert_eq!(
            std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
            "worker\n"
        );
        assert!(!worktree::rebase_in_progress(repo.path()));
    }

    #[test]
    fn refuses_to_merge_into_a_dirty_worker_workspace() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "worker"]);
        run(repo.path(), &["checkout", "-q", "-b", "labour", &base]);
        let commit = commit_file(repo.path(), "feature.txt", "feature\n", "labour work");
        run(repo.path(), &["checkout", "-q", "worker"]);
        std::fs::write(repo.path().join("README.md"), "dirty\n").unwrap();
        let error = merge_labour_commit(
            repo.path(),
            &commit,
            "merge labour",
            &["feature.txt".to_string()],
        )
        .unwrap_err();
        let typed = error.downcast_ref::<AgentWorkspaceError>().unwrap();
        assert_eq!(typed.code, agent_workspace_codes::WORKSPACE_NOT_QUIESCENT);
    }

    #[test]
    fn lands_the_workspace_branch_and_is_idempotent() {
        let repo = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        let component_branch = "verlinpark/components/c-1--a-1";
        let session_branch = "verlinpark/sessions/s-1/worker-a-1";
        ensure_component_branch(repo.path(), component_branch, &base).unwrap();
        let component_worktree = ensure_component_worktree(
            repo.path(),
            data.path(),
            "prj-1",
            "a-1",
            component_branch,
            &base,
        )
        .unwrap();
        run(repo.path(), &["branch", session_branch, &base]);
        let session_worktree = ensure_worktree(
            repo.path(),
            &data.path().join("session-worktree"),
            session_branch,
            &base,
        )
        .unwrap();
        commit_file(
            &session_worktree.path,
            "feature.txt",
            "feature\n",
            "worker commit",
        );
        let workspace_head = worktree::head_commit(&session_worktree.path).unwrap();
        let outcome = land_component_branch(
            &component_worktree.path,
            component_branch,
            session_branch,
            std::slice::from_ref(&workspace_head),
            "land worker",
        )
        .unwrap();
        assert!(!outcome.already_up_to_date);
        assert_eq!(outcome.commits, vec![workspace_head.clone()]);
        assert_eq!(
            worktree::git_output(repo.path(), &["rev-parse", component_branch]).unwrap(),
            outcome.landed_commit
        );
        assert_eq!(
            std::fs::read_to_string(component_worktree.path.join("feature.txt")).unwrap(),
            "feature\n"
        );
        let again = land_component_branch(
            &component_worktree.path,
            component_branch,
            session_branch,
            &[],
            "land worker again",
        )
        .unwrap();
        assert!(again.already_up_to_date);
        assert_eq!(again.landed_commit, outcome.landed_commit);
    }

    #[test]
    fn land_applies_only_the_accepted_subset() {
        let repo = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        let component_branch = "verlinpark/components/c-1--a-1";
        let session_branch = "verlinpark/sessions/s-1/worker-a-1";
        ensure_component_branch(repo.path(), component_branch, &base).unwrap();
        let component_worktree = ensure_component_worktree(
            repo.path(),
            data.path(),
            "prj-1",
            "a-1",
            component_branch,
            &base,
        )
        .unwrap();
        run(repo.path(), &["branch", session_branch, &base]);
        let session_worktree = ensure_worktree(
            repo.path(),
            &data.path().join("session-worktree"),
            session_branch,
            &base,
        )
        .unwrap();
        let first = commit_file(
            &session_worktree.path,
            "accepted.txt",
            "accepted\n",
            "accepted commit",
        );
        let rejected = commit_file(
            &session_worktree.path,
            "rejected.txt",
            "rejected\n",
            "rejected commit",
        );
        let outcome = land_component_branch(
            &component_worktree.path,
            component_branch,
            session_branch,
            std::slice::from_ref(&first),
            "land accepted subset",
        )
        .unwrap();
        assert_eq!(outcome.commits, vec![first]);
        assert_eq!(outcome.skipped_commits, vec![rejected.clone()]);
        assert!(
            component_worktree.path.join("accepted.txt").is_file(),
            "accepted work must land"
        );
        assert!(
            !component_worktree.path.join("rejected.txt").exists(),
            "a rejected commit must not ride along with the accepted subset"
        );
        // The rejected commit remains evidence on the workspace branch.
        assert!(
            worktree::is_ancestor(repo.path(), &rejected, session_branch).unwrap(),
            "workspace branch keeps the rejected commit"
        );
    }

    #[test]
    fn failed_land_rolls_back_to_the_previous_component_commit() {
        let repo = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        let component_branch = "verlinpark/components/c-1--a-1";
        let session_branch = "verlinpark/sessions/s-1/worker-a-1";
        ensure_component_branch(repo.path(), component_branch, &base).unwrap();
        let component_worktree = ensure_component_worktree(
            repo.path(),
            data.path(),
            "prj-1",
            "a-1",
            component_branch,
            &base,
        )
        .unwrap();
        let component_head = commit_file(
            &component_worktree.path,
            "README.md",
            "component\n",
            "component change",
        );
        // Create a conflicting session branch without checking it out in the
        // component worktree.
        let session_worktree = ensure_worktree(
            repo.path(),
            &data.path().join("session-worktree"),
            session_branch,
            &base,
        )
        .unwrap();
        commit_file(
            &session_worktree.path,
            "README.md",
            "session\n",
            "session change",
        );
        let session_head = worktree::head_commit(&session_worktree.path).unwrap();
        let error = land_component_branch(
            &component_worktree.path,
            component_branch,
            session_branch,
            &[session_head],
            "land worker",
        )
        .unwrap_err();
        let typed = error.downcast_ref::<AgentWorkspaceError>().unwrap();
        assert_eq!(typed.code, agent_workspace_codes::WORKSPACE_LAND_CONFLICT);
        assert_eq!(
            worktree::head_commit(&component_worktree.path).unwrap(),
            component_head
        );
        assert_eq!(
            std::fs::read_to_string(component_worktree.path.join("README.md")).unwrap(),
            "component\n"
        );
    }

    #[test]
    fn unaccepted_commits_are_detected() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let base = worktree::head_commit(repo.path()).unwrap();
        run(repo.path(), &["checkout", "-q", "-b", "workspace", &base]);
        let commit = commit_file(repo.path(), "feature.txt", "feature\n", "worker");
        run(repo.path(), &["checkout", "-q", "-b", "session", &base]);
        let unaccepted = unaccepted_commits(repo.path(), &base, "workspace", "session").unwrap();
        assert_eq!(unaccepted, vec![commit.clone()]);
        run(
            repo.path(),
            &["merge", "--no-ff", "workspace", "-m", "integrate"],
        );
        let accepted = unaccepted_commits(repo.path(), &base, "workspace", "session").unwrap();
        assert!(accepted.is_empty());
    }

    fn workspace(id: &str, status: &str) -> AgentWorkspace {
        AgentWorkspace {
            id: id.to_string(),
            project_id: "prj-1".to_string(),
            identity_id: "agt-1".to_string(),
            component: "auth".to_string(),
            branch: component_branch_name("agt-1", "auth"),
            base_commit: "a".repeat(40),
            worktree_path: "/tmp/workspace".to_string(),
            status: status.to_string(),
            created_at: "2026-11-13T00:00:00Z".to_string(),
            updated_at: "2026-11-13T00:00:00Z".to_string(),
            last_session_id: Some("ses-1".to_string()),
            lease_active: false,
            lease_expires_at: None,
            origin: "local".to_string(),
        }
    }

    fn session(id: &str, workspace_id: &str, status: &str) -> SessionWorkerWorkspace {
        SessionWorkerWorkspace {
            id: id.to_string(),
            workspace_id: workspace_id.to_string(),
            session_id: "ses-1".to_string(),
            identity_id: "agt-1".to_string(),
            worktree_path: "/tmp/session".to_string(),
            branch: session_workspace_branch_name("ses-1", "agt-1"),
            base_commit: "a".repeat(40),
            status: status.to_string(),
            created_at: "2026-11-13T00:00:00Z".to_string(),
            updated_at: "2026-11-13T00:00:00Z".to_string(),
            lease_active: false,
            lease_expires_at: None,
        }
    }

    fn event(
        id: &str,
        seq: u64,
        workspace: &AgentWorkspace,
        session: Option<&SessionWorkerWorkspace>,
    ) -> AgentWorkspaceEvent {
        let mut payload = serde_json::json!({"workspace": workspace});
        if let Some(session) = session {
            payload["session"] = serde_json::to_value(session).unwrap();
        }
        AgentWorkspaceEvent {
            id: id.to_string(),
            project_id: workspace.project_id.clone(),
            seq,
            kind: "agent.workspace.created".to_string(),
            payload,
            created_at: "2026-11-13T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn replay_folds_latest_snapshots_and_rejects_drift() {
        let mut first = workspace("wsp-1", "active");
        first.lease_active = true;
        let binding = session("sww-1", "wsp-1", "active");
        let mut second = first.clone();
        second.status = "archived".to_string();
        second.lease_active = false;
        let projection = project_from_events(&[
            event("evt-1", 1, &first, Some(&binding)),
            event("evt-2", 2, &second, None),
        ])
        .unwrap();
        assert_eq!(projection.workspaces.len(), 1);
        assert_eq!(projection.workspaces[0].status, "archived");
        assert_eq!(projection.sessions.len(), 1);
        let mismatches =
            compare_workspace_projection(&projection, std::slice::from_ref(&first), &[]);
        assert!(!mismatches.is_empty());
        assert!(
            mismatches
                .iter()
                .any(|item| item.contains("does not match"))
        );

        let duplicate = event("evt-3", 1, &first, None);
        let error = project_from_events(&[event("evt-1", 1, &first, None), duplicate]).unwrap_err();
        let typed = error.downcast_ref::<AgentWorkspaceError>().unwrap();
        assert_eq!(typed.code, agent_workspace_codes::WORKSPACE_INVALID_EVENT);
    }

    #[test]
    fn two_leased_workspaces_on_one_component_fail_replay() {
        let mut first = workspace("wsp-1", "active");
        first.lease_active = true;
        let mut second = workspace("wsp-2", "active");
        second.lease_active = true;
        second.identity_id = "agt-2".to_string();
        let error = project_from_events(&[
            event("evt-1", 1, &first, None),
            event("evt-2", 2, &second, None),
        ])
        .unwrap_err();
        let typed = error.downcast_ref::<AgentWorkspaceError>().unwrap();
        assert_eq!(typed.code, agent_workspace_codes::WORKSPACE_CONFLICT);
    }
}

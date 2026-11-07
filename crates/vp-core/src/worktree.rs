use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

/// Create (or reattach) the Leader worktree for a session.
///
/// Layout: `<data>/worktrees/<project>/<session>/leader`, branch
/// `verlinpark/<session-id>`. The worktree lives outside the repository so it
/// is never picked up by the user's search or commits.
pub fn create(
    project_root: &Path,
    data_dir: &Path,
    project_id: &str,
    session_id: &str,
) -> Result<WorktreeInfo> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    // UUIDv7 prefixes encode the creation timestamp, so two sessions created
    // close together can share the first 8 characters. Use the full id.
    let branch = format!("verlinpark/{session_id}");
    let path = data_dir
        .join("worktrees")
        .join(project_id)
        .join(session_id)
        .join("leader");
    let base_commit = git_output(&root, &["rev-parse", "HEAD"])?;

    if path.join(".git").is_file() {
        return Ok(WorktreeInfo {
            path,
            branch,
            base_commit,
        });
    }

    run_git(&root, &["worktree", "prune"])?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree directory {}", parent.display()))?;
    }

    if branch_exists(&root, &branch)? {
        run_git(
            &root,
            &[
                "worktree",
                "add",
                path.to_str()
                    .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?,
                &branch,
            ],
        )?;
    } else {
        run_git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str()
                    .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?,
                "HEAD",
            ],
        )?;
    }

    Ok(WorktreeInfo {
        path,
        branch,
        base_commit,
    })
}

/// Create a session worktree at an exact commit. Used by session forks so the
/// new branch starts from the checkpoint's committed tree, not current HEAD.
pub fn create_from_commit(
    project_root: &Path,
    data_dir: &Path,
    project_id: &str,
    session_id: &str,
    commit: &str,
) -> Result<WorktreeInfo> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    let branch = format!("verlinpark/{session_id}");
    let path = data_dir
        .join("worktrees")
        .join(project_id)
        .join(session_id)
        .join("leader");
    let resolved = git_output(
        &root,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )
    .with_context(|| format!("resolving checkpoint commit {commit}"))?;

    if path.join(".git").is_file() {
        return Ok(WorktreeInfo {
            path,
            branch,
            base_commit: resolved,
        });
    }

    run_git(&root, &["worktree", "prune"])?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree directory {}", parent.display()))?;
    }
    if branch_exists(&root, &branch)? {
        bail!("fork branch already exists: {branch}");
    }
    run_git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str()
                .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?,
            &resolved,
        ],
    )?;
    Ok(WorktreeInfo {
        path,
        branch,
        base_commit: resolved,
    })
}

/// Porcelain status for a worktree. Ignored files do not appear, so normal
/// build outputs do not block checkpoints.
pub fn status_porcelain(worktree: &Path) -> Result<String> {
    git_output(worktree, &["status", "--porcelain"])
}

/// Current branch name for a session worktree.
pub fn current_branch(worktree: &Path) -> Result<String> {
    git_output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Delete the checkpoint ref that keeps a dirty snapshot commit reachable.
/// Clean checkpoints point at the session branch commit and have no ref.
pub fn delete_checkpoint_ref(
    project_root: &Path,
    session_id: &str,
    checkpoint_id: &str,
) -> Result<()> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    let reference = format!("refs/verlinpark/checkpoints/{session_id}/{checkpoint_id}");
    let status = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .stdout(std::process::Stdio::null())
        .status()
        .context("checking for an existing checkpoint ref")?;
    if status.success() {
        run_git(&root, &["update-ref", "-d", &reference])?;
    }
    Ok(())
}

/// Snapshot the worktree, including uncommitted tracked changes and untracked
/// non-ignored files, into a commit object without touching the real index or
/// HEAD. The commit stays reachable through a checkpoint ref so Git GC cannot
/// prune it before a fork.
pub fn snapshot_worktree(
    worktree: &Path,
    data_dir: &Path,
    session_id: &str,
    checkpoint_id: &str,
    label: &str,
) -> Result<String> {
    let head = head_commit(worktree)?;
    let temp_dir = data_dir.join("tmp");
    std::fs::create_dir_all(&temp_dir)
        .with_context(|| format!("creating {}", temp_dir.display()))?;
    let index_path = temp_dir.join(format!("checkpoint-{checkpoint_id}.index"));
    let _ = std::fs::remove_file(&index_path);
    let result = (|| -> Result<String> {
        git_output_with_index(worktree, &index_path, &["read-tree", "HEAD"])?;
        git_output_with_index(worktree, &index_path, &["add", "-A"])?;
        let tree = git_output_with_index(worktree, &index_path, &["write-tree"])?;
        let head_tree = git_output(worktree, &["rev-parse", "HEAD^{tree}"])?;
        if tree == head_tree {
            return Ok(head);
        }
        let message = format!("VerlinPark checkpoint {checkpoint_id}: {label}");
        let commit = git_output_with_index(
            worktree,
            &index_path,
            &["commit-tree", &tree, "-p", &head, "-m", &message],
        )?;
        let reference = format!("refs/verlinpark/checkpoints/{session_id}/{checkpoint_id}");
        git_output_with_index(worktree, &index_path, &["update-ref", &reference, &commit])?;
        Ok(commit)
    })();
    let _ = std::fs::remove_file(&index_path);
    result
}

fn git_output_with_index(root: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_INDEX_FILE", index)
        .env("GIT_AUTHOR_NAME", "VerlinPark")
        .env("GIT_AUTHOR_EMAIL", "verlinpark@localhost")
        .env("GIT_COMMITTER_NAME", "VerlinPark")
        .env("GIT_COMMITTER_EMAIL", "verlinpark@localhost")
        .output()
        .context("running git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("git {} failed: {detail}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Create a Worker worktree based on the session branch.
pub fn create_task(
    project_root: &Path,
    data_dir: &Path,
    project_id: &str,
    session_id: &str,
    task_id: &str,
    base_branch: &str,
) -> Result<WorktreeInfo> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    // A slash would make the task branch a child ref of the session branch,
    // which Git refuses (a ref cannot be both a file and a directory).
    let branch = format!("verlinpark/{session_id}--{task_id}");
    let path = data_dir
        .join("worktrees")
        .join(project_id)
        .join(session_id)
        .join(task_id);
    let base_commit = if path.join(".git").is_file() {
        git_output(&root, &["merge-base", base_branch, &branch])
            .or_else(|_| git_output(&root, &["rev-parse", base_branch]))?
    } else {
        git_output(&root, &["rev-parse", base_branch])?
    };

    if path.join(".git").is_file() {
        return Ok(WorktreeInfo {
            path,
            branch,
            base_commit,
        });
    }

    run_git(&root, &["worktree", "prune"])?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree directory {}", parent.display()))?;
    }

    if branch_exists(&root, &branch)? {
        run_git(
            &root,
            &[
                "worktree",
                "add",
                path.to_str()
                    .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?,
                &branch,
            ],
        )?;
    } else {
        run_git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str()
                    .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?,
                base_branch,
            ],
        )?;
    }

    Ok(WorktreeInfo {
        path,
        branch,
        base_commit,
    })
}

pub fn changed_paths(worktree: &Path, base: &str) -> Result<Vec<String>> {
    let tracked = git_output(worktree, &["diff", "--name-only", base])?;
    let untracked = git_output(worktree, &["ls-files", "--others", "--exclude-standard"])?;
    let mut paths = tracked
        .lines()
        .chain(untracked.lines())
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub fn commit_all(worktree: &Path, message: &str) -> Result<Option<String>> {
    let status = git_output(worktree, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(None);
    }
    run_git(worktree, &["add", "-A"])?;
    run_git(worktree, &["commit", "-m", message])?;
    Ok(Some(git_output(worktree, &["rev-parse", "HEAD"])?))
}

pub fn merge_branch(session_worktree: &Path, branch: &str, message: &str) -> Result<()> {
    match run_git(
        session_worktree,
        &["merge", "--no-ff", branch, "-m", message],
    ) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = run_git(session_worktree, &["merge", "--abort"]);
            Err(error.context("integrating task branch"))
        }
    }
}

#[derive(Debug, Clone)]
pub struct ForkMergeOutcome {
    pub merged: bool,
    pub conflicts: Vec<String>,
    pub detail: String,
}

/// Merge a fork branch into a session worktree. Conflicts are returned as a
/// normal outcome after aborting the merge; branch/checkout failures remain
/// errors. This keeps conflict reporting separate from infrastructure errors.
pub fn merge_fork_branch(
    session_worktree: &Path,
    branch: &str,
    message: &str,
) -> Result<ForkMergeOutcome> {
    let output = Command::new("git")
        .arg("-C")
        .arg(session_worktree)
        .args(["merge", "--no-ff", branch, "-m", message])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("merging fork branch")?;
    if output.status.success() {
        return Ok(ForkMergeOutcome {
            merged: true,
            conflicts: Vec::new(),
            detail: String::new(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    let conflicts = git_output(
        session_worktree,
        &["diff", "--name-only", "--diff-filter=U"],
    )
    .unwrap_or_default()
    .lines()
    .filter(|line| !line.trim().is_empty())
    .map(str::to_string)
    .collect::<Vec<_>>();
    let _ = run_git(session_worktree, &["merge", "--abort"]);
    if !conflicts.is_empty()
        || detail.contains("CONFLICT")
        || detail.contains("Automatic merge failed")
    {
        Ok(ForkMergeOutcome {
            merged: false,
            conflicts,
            detail,
        })
    } else {
        Err(anyhow!("fork merge failed: {detail}"))
    }
}

/// Create a worktree for an imported session at an explicit path. If the
/// session branch already exists it is attached; otherwise it is created at the
/// bundle's base commit.
pub fn rehydrate(
    project_root: &Path,
    worktree: &Path,
    branch: &str,
    base_commit: &str,
) -> Result<()> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    if worktree.exists() {
        anyhow::bail!("worktree path already exists: {}", worktree.display());
    }
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree directory {}", parent.display()))?;
    }
    run_git(&root, &["worktree", "prune"])?;
    let path = worktree
        .to_str()
        .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;
    if branch_exists(&root, branch)? {
        run_git(&root, &["worktree", "add", path, branch])?;
    } else if base_commit.trim().is_empty() {
        run_git(&root, &["worktree", "add", "-b", branch, path, "HEAD"])?;
    } else {
        run_git(&root, &["worktree", "add", "-b", branch, path, base_commit])?;
    }
    Ok(())
}

pub fn rebase_onto(worktree: &Path, onto: &str) -> Result<()> {
    match run_git(worktree, &["rebase", onto]) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = run_git(worktree, &["rebase", "--abort"]);
            Err(error.context("rebasing task branch"))
        }
    }
}

/// Start a rebase and leave conflicts in the worktree for an agent to resolve.
pub fn rebase_onto_keep_conflicts(worktree: &Path, onto: &str) -> Result<()> {
    run_git(worktree, &["rebase", onto]).context("rebasing task branch")
}

pub fn rebase_in_progress(worktree: &Path) -> bool {
    for name in ["rebase-merge", "rebase-apply"] {
        if let Ok(path) = git_output(worktree, &["rev-parse", "--git-path", name]) {
            let path = PathBuf::from(path.trim());
            let path = if path.is_absolute() {
                path
            } else {
                worktree.join(path)
            };
            if path.is_dir() {
                return true;
            }
        }
    }
    false
}

pub fn conflicted_paths(worktree: &Path) -> Result<Vec<String>> {
    let output = git_output(worktree, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn stage_all(worktree: &Path) -> Result<()> {
    run_git(worktree, &["add", "-A"])
}

/// Detect leftover conflict markers in files staged for the in-progress rebase.
pub fn conflict_markers(worktree: &Path) -> Result<Vec<String>> {
    let output = git_output(worktree, &["diff", "--cached", "--name-only"])?;
    let mut markers = Vec::new();
    for path in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let file = worktree.join(path);
        let Ok(metadata) = std::fs::metadata(&file) else {
            continue;
        };
        if metadata.len() > 2 * 1024 * 1024 {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        let has_start = content.lines().any(|line| line.starts_with("<<<<<<<"));
        let has_sep = content.lines().any(|line| line.starts_with("======="));
        let has_end = content.lines().any(|line| line.starts_with(">>>>>>>"));
        if has_start && has_sep && has_end {
            markers.push(path.to_string());
        }
    }
    Ok(markers)
}

pub fn continue_rebase(worktree: &Path) -> Result<()> {
    run_git(
        worktree,
        &["-c", "core.editor=true", "rebase", "--continue"],
    )
    .context("continuing rebase")
}

pub fn abort_rebase(worktree: &Path) {
    let _ = run_git(worktree, &["rebase", "--abort"]);
}

pub fn head_commit(worktree: &Path) -> Result<String> {
    git_output(worktree, &["rev-parse", "HEAD"])
}

pub fn reset_hard(worktree: &Path, commit: &str) -> Result<()> {
    run_git(worktree, &["reset", "--hard", commit])
}

/// Check the merge result for whitespace errors and conflict markers before it
/// becomes the session branch. This is deliberately language-agnostic; projects
/// can add a real test command through `VERLINPARK_INTEGRATION_COMMAND`.
pub fn check_merged_diff(worktree: &Path, base_commit: &str) -> Result<()> {
    match git_output(worktree, &["diff", "--check", base_commit, "HEAD"]) {
        Ok(output) if output.trim().is_empty() => Ok(()),
        Ok(output) => Err(anyhow!("integration diff check failed:\n{output}")),
        Err(error) => Err(error.context("integration diff check failed")),
    }
}

/// Remove only the worktree, keeping its branch. Used by explicit cleanup so
/// unlanded commits remain recoverable.
pub fn remove_worktree(project_root: &Path, worktree: &Path) -> Result<()> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    run_git(
        &root,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    )
}

pub fn delete_branch(project_root: &Path, branch: &str) -> Result<()> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    run_git(&root, &["branch", "-D", branch])
}

pub fn has_uncommitted_changes(worktree: &Path) -> Result<bool> {
    Ok(!git_output(worktree, &["status", "--porcelain"])?
        .trim()
        .is_empty())
}

pub fn remove(project_root: &Path, worktree: &Path, branch: &str) {
    if let Ok(root) = project_root.canonicalize() {
        let _ = run_git(
            &root,
            &["worktree", "remove", "--force", &worktree.to_string_lossy()],
        );
        let _ = run_git(&root, &["branch", "-D", branch]);
    }
}

pub fn branch_exists(root: &Path, branch: &str) -> Result<bool> {
    let reference = format!("refs/heads/{branch}");
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .status()
        .context("checking for an existing branch")?;
    Ok(status.success())
}

/// Tracked files changed in a worktree. Untracked build/test outputs are
/// ignored so a read-only Verifier can still run tests.
/// Create an isolated detached clone of one committed task tree for Verifier
/// runs. The clone has its own refs/Git metadata and is discarded afterwards, so
/// Verifier shell activity cannot mutate the Worker worktree or branch.
pub fn create_verification_checkout(
    project_root: &Path,
    data_dir: &Path,
    session_id: &str,
    commit: &str,
) -> Result<PathBuf> {
    let root = project_root
        .canonicalize()
        .with_context(|| format!("resolving project root {}", project_root.display()))?;
    let path = data_dir
        .join("verify")
        .join(session_id)
        .join(Uuid::now_v7().to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating verification directory {}", parent.display()))?;
    }
    let output = Command::new("git")
        .arg("clone")
        .arg("--shared")
        .arg("--no-checkout")
        .arg(&root)
        .arg(&path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("cloning verification checkout")?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&path);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("git clone verification checkout failed: {stderr}");
    }
    if let Err(error) = git_output(&path, &["checkout", "--detach", commit]) {
        let _ = std::fs::remove_dir_all(&path);
        return Err(error).context("checking out verification commit");
    }
    Ok(path)
}

pub fn tracked_changes(root: &Path) -> Result<Vec<String>> {
    let output = git_output(root, &["status", "--porcelain", "--untracked-files=no"])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Discard tracked modifications made by a Verifier. Untracked files are left
/// alone because they may be legitimate test artifacts.
pub fn reset_tracked_changes(root: &Path) -> Result<()> {
    run_git(root, &["reset", "--hard", "HEAD"])
}

pub(crate) fn git_output(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!("git {} failed: {detail}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    git_output(root, args).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(path: &Path) {
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["init", "-q"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.email", "test@verlinpark.local"])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.name", "VerlinPark Test"])
            .status()
            .unwrap();
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "."])
            .status()
            .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["commit", "-q", "-m", "initial"])
            .status()
            .unwrap();
    }

    #[test]
    fn creates_an_isolated_verification_checkout() {
        let repo = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        init_repo(repo.path());
        std::fs::write(repo.path().join("tracked.txt"), "committed\n").unwrap();
        run_git(repo.path(), &["add", "tracked.txt"]).unwrap();
        run_git(repo.path(), &["commit", "-q", "-m", "tracked"]).unwrap();
        let commit = head_commit(repo.path()).unwrap();
        let checkout =
            create_verification_checkout(repo.path(), data.path(), "session-verify", &commit)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(checkout.join("tracked.txt")).unwrap(),
            "committed\n"
        );
        std::fs::write(checkout.join("verifier-only.txt"), "ephemeral\n").unwrap();
        assert!(!repo.path().join("verifier-only.txt").exists());
        assert!(checkout.join("verifier-only.txt").exists());
        run_git(&checkout, &["add", "verifier-only.txt"]).unwrap();
        run_git(&checkout, &["commit", "-q", "-m", "verifier mutation"]).unwrap();
        assert_eq!(head_commit(repo.path()).unwrap(), commit);
        assert_ne!(head_commit(&checkout).unwrap(), commit);
        std::fs::remove_dir_all(checkout).unwrap();
    }

    #[test]
    fn creates_a_session_worktree_and_branch() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let info = create(repo.path(), data.path(), "project-1", "session-1").unwrap();
        assert!(info.path.join("README.md").is_file());
        assert!(info.branch.starts_with("verlinpark/"));
        assert_eq!(info.base_commit.len(), 40);
        let branch = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_ne!(String::from_utf8_lossy(&branch.stdout).trim(), info.branch);
    }

    #[test]
    fn creates_and_integrates_a_task_worktree() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let session = create(repo.path(), data.path(), "project-1", "session-1").unwrap();
        let task = create_task(
            repo.path(),
            data.path(),
            "project-1",
            "session-1",
            "task-1",
            &session.branch,
        )
        .unwrap();
        std::fs::write(task.path.join("worker.txt"), "from worker\n").unwrap();
        let commit = commit_all(&task.path, "task work").unwrap().unwrap();
        assert_eq!(commit.len(), 40);
        merge_branch(&session.path, &task.branch, "merge task").unwrap();
        assert_eq!(
            std::fs::read_to_string(session.path.join("worker.txt")).unwrap(),
            "from worker\n"
        );
    }

    #[test]
    fn reattaches_an_existing_task_branch_and_keeps_the_branch_point() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let session = create(repo.path(), data.path(), "project-1", "session-1").unwrap();
        let task = create_task(
            repo.path(),
            data.path(),
            "project-1",
            "session-1",
            "task-1",
            &session.branch,
        )
        .unwrap();
        std::fs::write(
            task.path.join("worker.txt"),
            "from worker
",
        )
        .unwrap();
        commit_all(&task.path, "task work").unwrap();
        let branch_point = git_output(repo.path(), &["rev-parse", &session.branch]).unwrap();
        remove_worktree(repo.path(), &task.path).unwrap();
        assert!(!task.path.exists());
        assert!(branch_exists(repo.path(), &task.branch).unwrap());

        let reattached = create_task(
            repo.path(),
            data.path(),
            "project-1",
            "session-1",
            "task-1",
            &session.branch,
        )
        .unwrap();
        assert!(reattached.path.join(".git").is_file());
        assert_eq!(reattached.branch, task.branch);
        assert_eq!(reattached.base_commit, branch_point);
        assert_eq!(
            std::fs::read_to_string(reattached.path.join("worker.txt")).unwrap(),
            "from worker\n"
        );
    }

    #[test]
    fn resolves_a_rebase_conflict_with_staged_files() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let session = create(repo.path(), data.path(), "project-1", "session-1").unwrap();
        let task = create_task(
            repo.path(),
            data.path(),
            "project-1",
            "session-1",
            "task-1",
            &session.branch,
        )
        .unwrap();
        std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
        commit_all(&task.path, "worker change").unwrap();
        std::fs::write(session.path.join("README.md"), "session\n").unwrap();
        commit_all(&session.path, "session change").unwrap();

        assert!(rebase_onto_keep_conflicts(&task.path, &session.branch).is_err());
        assert!(rebase_in_progress(&task.path));
        assert_eq!(
            conflicted_paths(&task.path).unwrap(),
            vec!["README.md".to_string()]
        );

        std::fs::write(task.path.join("README.md"), "resolved\n").unwrap();
        stage_all(&task.path).unwrap();
        assert!(conflict_markers(&task.path).unwrap().is_empty());
        continue_rebase(&task.path).unwrap();
        assert!(!rebase_in_progress(&task.path));
        assert_eq!(
            std::fs::read_to_string(task.path.join("README.md")).unwrap(),
            "resolved\n"
        );
    }

    #[test]
    fn detects_conflict_markers_in_staged_files() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let session = create(repo.path(), data.path(), "project-1", "session-1").unwrap();
        let task = create_task(
            repo.path(),
            data.path(),
            "project-1",
            "session-1",
            "task-1",
            &session.branch,
        )
        .unwrap();
        std::fs::write(task.path.join("conflict.txt"), "before\n").unwrap();
        commit_all(&task.path, "add conflict file").unwrap();
        std::fs::write(
            task.path.join("conflict.txt"),
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> task\n",
        )
        .unwrap();
        stage_all(&task.path).unwrap();
        assert_eq!(
            conflict_markers(&task.path).unwrap(),
            vec!["conflict.txt".to_string()]
        );
    }

    #[test]
    fn fails_for_a_non_git_directory() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let error = create(repo.path(), data.path(), "project-1", "session-1").unwrap_err();
        assert!(error.to_string().contains("git"));
    }
}

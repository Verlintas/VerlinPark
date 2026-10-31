use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

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
    let base_commit = git_output(&root, &["rev-parse", base_branch])?;

    if path.join(".git").is_file() {
        return Ok(WorktreeInfo {
            path,
            branch,
            base_commit,
        });
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating worktree directory {}", parent.display()))?;
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
            base_branch,
        ],
    )?;

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

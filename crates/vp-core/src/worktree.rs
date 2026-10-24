use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
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

    if path.join(".git").is_file() {
        return Ok(WorktreeInfo { path, branch });
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

    Ok(WorktreeInfo { path, branch })
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

fn branch_exists(root: &Path, branch: &str) -> Result<bool> {
    let reference = format!("refs/heads/{branch}");
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .status()
        .context("checking for an existing branch")?;
    Ok(status.success())
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
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
    Ok(())
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
        let branch = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert_ne!(String::from_utf8_lossy(&branch.stdout).trim(), info.branch);
    }

    #[test]
    fn fails_for_a_non_git_directory() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let error = create(repo.path(), data.path(), "project-1", "session-1").unwrap_err();
        assert!(error.to_string().contains("git"));
    }
}

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use vp_protocol::types::{GitCommit, GitShowResult};

const MAX_LOG_LIMIT: usize = 200;
const MAX_SHOW_BYTES: usize = 200_000;

pub fn log(root: &Path, limit: u32) -> Result<Vec<GitCommit>> {
    let limit = usize::try_from(limit)
        .unwrap_or(MAX_LOG_LIMIT)
        .clamp(1, MAX_LOG_LIMIT);
    let output = git(
        root,
        &[
            "log",
            "--no-show-signature",
            "--date=iso-strict",
            "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%D",
            "-n",
            &limit.to_string(),
        ],
    )?;
    let text = String::from_utf8_lossy(&output);
    let commits = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let fields = line.split('\u{1f}').collect::<Vec<_>>();
            if fields.len() < 7 {
                return None;
            }
            Some(GitCommit {
                hash: fields[0].to_string(),
                short_hash: fields[1].to_string(),
                author: fields[2].to_string(),
                email: fields[3].to_string(),
                author_date: fields[4].to_string(),
                subject: fields[5].to_string(),
                refs: fields[6].to_string(),
            })
        })
        .collect();
    Ok(commits)
}

pub fn show(root: &Path, commit: &str) -> Result<GitShowResult> {
    if commit.len() < 4
        || commit.len() > 40
        || !commit
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("commit must be a 4-40 character hexadecimal hash");
    }
    let output = git(
        root,
        &[
            "show",
            "--no-color",
            "--no-ext-diff",
            "--no-show-signature",
            "--stat",
            "--patch",
            "--format=fuller",
            commit,
            "--",
        ],
    )?;
    let mut text = String::from_utf8_lossy(&output).to_string();
    let truncated = text.len() > MAX_SHOW_BYTES;
    if truncated {
        text.truncate(MAX_SHOW_BYTES);
        text.push_str("\n… output truncated");
    }
    Ok(GitShowResult {
        session_id: String::new(),
        commit: commit.to_string(),
        output: text,
        truncated,
    })
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().copied().unwrap_or("command"),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test User"]);
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "-qm", "initial commit"]);
        std::fs::write(dir.path().join("README.md"), "hello again\n").unwrap();
        git(&["commit", "-qam", "second commit"]);
        dir
    }

    #[test]
    fn lists_and_shows_commits() {
        let dir = repo();
        let commits = log(dir.path(), 10).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "second commit");
        assert_eq!(commits[1].subject, "initial commit");
        assert_eq!(commits[0].author, "Test User");

        let show = show(dir.path(), &commits[0].hash).unwrap();
        assert!(show.output.contains("second commit"));
        assert!(show.output.contains("hello again"));
        assert!(!show.truncated);
    }

    #[test]
    fn rejects_non_hex_commits() {
        let dir = repo();
        assert!(show(dir.path(), "--help").is_err());
        assert!(show(dir.path(), "not-a-hash").is_err());
    }
}

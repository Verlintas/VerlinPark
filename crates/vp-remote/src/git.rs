//! Structured Git operations executed on the remote host.
//!
//! The helper never accepts arbitrary Git arguments. Every operation builds a
//! fixed argv from validated refs and paths, disables external diff drivers and
//! hooks, and caps output. Worktree paths are resolved through the same path
//! jail as file writes.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::jail::PathJail;
use crate::protocol::{
    DiffScope, GitDiffParams, GitDiffResult, GitLogEntry, GitLogParams, GitLogResult,
    GitRevParseParams, GitRevParseResult, GitStatusEntry, GitStatusResult, GitWorktreeAddParams,
    GitWorktreeAddResult, GitWorktreeListResult, GitWorktreeRemoveParams, GitWorktreeRemoveResult,
    MAX_GIT_OUTPUT_BYTES, MAX_GIT_PATHS, RemoteError, RemoteErrorCode, WorktreeEntry,
};

const GIT_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_STATUS_ENTRIES: usize = 2_000;
const MAX_LOG_LIMIT: u32 = 200;
const MAX_PATCH_CHARS: usize = 1_000_000;

#[derive(Debug, Clone)]
struct GitOutput {
    stdout: String,
    stderr: String,
    success: bool,
    truncated: bool,
}

fn git_environment(command: &mut Command, root: &Path) {
    command.env_clear();
    for name in [
        "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    if std::env::var_os("PATH").is_none() {
        command.env(
            "PATH",
            "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        );
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env(
            "HOME",
            std::env::var_os("HOME").unwrap_or_else(|| root.into()),
        );
    command.current_dir(root);
}

async fn run_git(root: &Path, args: &[String]) -> Result<GitOutput, RemoteError> {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    git_environment(&mut command, root);
    let child = command.spawn().map_err(|error| {
        RemoteError::new(
            RemoteErrorCode::GitUnavailable,
            format!("cannot run git on the remote host: {error}"),
        )
        .with_hint("install git or use the file tools")
    })?;
    let output = match timeout(GIT_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(RemoteError::new(
                RemoteErrorCode::GitFailed,
                format!("git process failed: {error}"),
            ));
        }
        Err(_) => {
            return Err(RemoteError::new(
                RemoteErrorCode::Timeout,
                format!("git command exceeded {} seconds", GIT_TIMEOUT.as_secs()),
            ));
        }
    };
    let stdout = output.stdout;
    let truncated = stdout.len() > MAX_GIT_OUTPUT_BYTES as usize;
    let stdout =
        String::from_utf8_lossy(&stdout[..stdout.len().min(MAX_GIT_OUTPUT_BYTES as usize)])
            .to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(GitOutput {
        stdout,
        stderr,
        success: output.status.success(),
        truncated,
    })
}

fn git_error(action: &str, output: &GitOutput) -> RemoteError {
    let detail = output.stderr.trim();
    RemoteError::new(
        RemoteErrorCode::GitFailed,
        if detail.is_empty() {
            format!("git {action} failed")
        } else {
            format!("git {action} failed: {}", truncate_chars(detail, 4_000))
        },
    )
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut rendered = value.chars().take(limit).collect::<String>();
    rendered.push_str("\n… truncated");
    rendered
}

fn validate_ref(value: &str, label: &str) -> Result<String, RemoteError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 512 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("{label} must be 1..=512 characters"),
        ));
    }
    if value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\0')
        || value.contains('\n')
        || value.contains('\r')
        || value.contains(char::is_whitespace)
        || value.contains("..")
        || value.contains("@{")
        || value.contains('\\')
        || value.contains(':')
        || value.contains('?')
        || value.contains('*')
        || value.contains('[')
        || value.contains('~')
        || value.contains('^')
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("{label} is not a safe Git revision: {value}"),
        ));
    }
    Ok(value.to_string())
}

/// Validate a commit-ish that may use Git's `~`/`^` traversal syntax. It is
/// passed as a separate argv element after a fixed subcommand, so the only
/// option-injection risk is a leading `-`, which remains rejected.
fn validate_revision(value: &str, label: &str) -> Result<String, RemoteError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 512 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("{label} must be 1..=512 characters"),
        ));
    }
    if value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("..")
        || value.contains("@{")
        || value.contains('\\')
        || value.contains(':')
        || value.contains('?')
        || value.contains('*')
        || value.contains('[')
        || value.contains(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("{label} is not a safe Git revision: {value}"),
        ));
    }
    Ok(value.to_string())
}

fn validate_branch(value: &str) -> Result<String, RemoteError> {
    let value = validate_ref(value, "branch")?;
    if value.ends_with('.') || value.ends_with(".lock") || value.starts_with("refs/heads/") {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("invalid branch name: {value}"),
        ));
    }
    if value.split('/').any(str::is_empty) {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("invalid branch name: {value}"),
        ));
    }
    Ok(value)
}

fn validate_paths(paths: &[String]) -> Result<Vec<String>, RemoteError> {
    if paths.len() > MAX_GIT_PATHS {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("at most {MAX_GIT_PATHS} path filters are allowed"),
        ));
    }
    paths
        .iter()
        .map(|path| {
            if path.is_empty() || path.contains('\0') || path.contains('\n') || path.len() > 2048 {
                return Err(RemoteError::new(
                    RemoteErrorCode::InvalidParams,
                    "git path filters must be non-empty, under 2048 bytes, and free of control characters",
                ));
            }
            if Path::new(path).is_absolute() {
                return Err(RemoteError::new(
                    RemoteErrorCode::PathEscape,
                    "git path filters must be relative",
                ));
            }
            Ok(path.clone())
        })
        .collect()
}

pub async fn status(jail: &PathJail) -> Result<GitStatusResult, RemoteError> {
    let root = jail.root();
    let branch = run_git(
        root,
        &["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()],
    )
    .await
    .ok()
    .filter(|output| output.success)
    .map(|output| output.stdout.trim().to_string())
    .filter(|value| !value.is_empty());
    let head = run_git(root, &["rev-parse".into(), "HEAD".into()])
        .await
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout.trim().to_string())
        .filter(|value| !value.is_empty());
    let upstream = run_git(
        root,
        &[
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "--symbolic-full-name".into(),
            "@{u}".into(),
        ],
    )
    .await
    .ok()
    .filter(|output| output.success)
    .map(|output| output.stdout.trim().to_string())
    .filter(|value| !value.is_empty());
    let (ahead, behind) = if let Some(upstream) = upstream.as_deref() {
        run_git(
            root,
            &[
                "rev-list".into(),
                "--left-right".into(),
                "--count".into(),
                format!("HEAD...{upstream}"),
            ],
        )
        .await
        .ok()
        .filter(|output| output.success)
        .and_then(|output| {
            let mut parts = output.stdout.split_whitespace();
            let ahead = parts.next()?.parse().ok()?;
            let behind = parts.next()?.parse().ok()?;
            Some((ahead, behind))
        })
        .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    let status = run_git(
        root,
        &[
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=all".into(),
        ],
    )
    .await?;
    if !status.success {
        return Err(git_error("status", &status));
    }
    let entries = parse_status_entries(&status.stdout);
    Ok(GitStatusResult {
        clean: entries.is_empty(),
        branch,
        head,
        upstream,
        ahead,
        behind,
        entries,
    })
}

fn parse_status_entries(raw: &str) -> Vec<GitStatusEntry> {
    let mut entries = Vec::new();
    let mut parts = raw.split('\0').filter(|value| !value.is_empty());
    while let Some(record) = parts.next() {
        if record.len() < 4 {
            continue;
        }
        let status = record[..2].to_string();
        let path = record[3..].to_string();
        let original_path = if status.contains('R') || status.contains('C') {
            parts.next().map(str::to_string)
        } else {
            None
        };
        entries.push(GitStatusEntry {
            status,
            path,
            original_path,
        });
        if entries.len() >= MAX_STATUS_ENTRIES {
            break;
        }
    }
    entries
}

pub async fn diff(jail: &PathJail, params: &GitDiffParams) -> Result<GitDiffResult, RemoteError> {
    let root = jail.root();
    let paths = validate_paths(&params.paths)?;
    if params.context > 20 {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            "diff context must be 0..=20",
        ));
    }
    let mut args = vec!["--no-ext-diff".to_string(), "--no-color".to_string()];
    match params.scope {
        DiffScope::Unstaged => {
            args.insert(0, "diff".to_string());
        }
        DiffScope::Staged => {
            args.insert(0, "diff".to_string());
            args.push("--cached".to_string());
        }
        DiffScope::Base => {
            let base = validate_revision(params.base.as_deref().unwrap_or("HEAD"), "base")?;
            args.insert(0, "diff".to_string());
            args.push(format!("{base}...HEAD"));
        }
        DiffScope::Commit => {
            let commit = validate_revision(params.commit.as_deref().unwrap_or("HEAD"), "commit")?;
            args.insert(0, "show".to_string());
            args.push("--format=".to_string());
            args.push(commit);
        }
    }
    args.push(format!("--unified={}", params.context));
    if params.stat_only {
        args.push("--stat".to_string());
    }
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths);
    }
    let output = run_git(root, &args).await?;
    if !output.success {
        return Err(git_error("diff", &output));
    }
    let mut patch = output.stdout;
    let truncated = output.truncated || patch.chars().count() > MAX_PATCH_CHARS;
    if patch.chars().count() > MAX_PATCH_CHARS {
        patch = truncate_chars(&patch, MAX_PATCH_CHARS);
    }
    let mut stat_args = vec!["diff".to_string(), "--stat".to_string()];
    if matches!(params.scope, DiffScope::Staged) {
        stat_args.push("--cached".to_string());
    }
    let stat_output = run_git(root, &stat_args)
        .await
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout)
        .unwrap_or_default();
    Ok(GitDiffResult {
        scope: params.scope,
        patch,
        stat: stat_output,
        truncated,
    })
}

pub async fn log(jail: &PathJail, params: &GitLogParams) -> Result<GitLogResult, RemoteError> {
    if params.limit == 0 || params.limit > MAX_LOG_LIMIT {
        return Err(RemoteError::new(
            RemoteErrorCode::InvalidParams,
            format!("log limit must be 1..={MAX_LOG_LIMIT}"),
        ));
    }
    let paths = validate_paths(&params.paths)?;
    let mut args = vec![
        "log".to_string(),
        format!("-n{}", params.limit),
        "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%s".to_string(),
    ];
    if let Some(since) = params.since.as_deref() {
        let since = validate_revision(since, "since")?;
        args.push(format!("--since={since}"));
    }
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths);
    }
    let output = run_git(jail.root(), &args).await?;
    if !output.success {
        return Err(git_error("log", &output));
    }
    let entries = output
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(6, '\u{1f}');
            Some(GitLogEntry {
                hash: fields.next()?.to_string(),
                short_hash: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                email: fields.next()?.to_string(),
                timestamp: fields.next()?.parse().ok()?,
                subject: fields.next()?.to_string(),
            })
        })
        .collect::<Vec<_>>();
    Ok(GitLogResult {
        truncated: output.truncated || entries.len() >= params.limit as usize,
        entries,
    })
}

pub async fn rev_parse(
    jail: &PathJail,
    params: &GitRevParseParams,
) -> Result<GitRevParseResult, RemoteError> {
    let rev = validate_revision(&params.rev, "rev")?;
    let mut args = vec![
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
    ];
    args.push(if params.commit {
        format!("{rev}^{{commit}}")
    } else {
        rev.clone()
    });
    let output = run_git(jail.root(), &args).await?;
    if !output.success || output.stdout.trim().is_empty() {
        return Err(RemoteError::new(
            RemoteErrorCode::PathNotFound,
            format!("Git revision does not exist: {rev}"),
        ));
    }
    Ok(GitRevParseResult {
        rev,
        value: output.stdout.trim().to_string(),
    })
}

pub async fn worktree_add(
    jail: &PathJail,
    params: &GitWorktreeAddParams,
) -> Result<GitWorktreeAddResult, RemoteError> {
    let branch = validate_branch(&params.branch)?;
    let start_from =
        validate_revision(params.start_from.as_deref().unwrap_or("HEAD"), "start_from")?;
    let target = jail.resolve_worktree(&params.path)?;
    if target.exists() {
        return Err(RemoteError::new(
            RemoteErrorCode::AlreadyExists,
            format!("worktree path already exists: {}", target.display()),
        )
        .with_hint("remove the worktree or choose another path"));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::Internal,
                format!(
                    "cannot create worktree parent {}: {error}",
                    parent.display()
                ),
            )
        })?;
    }
    let target_string = target.to_string_lossy().to_string();
    let mut args = vec!["worktree".to_string(), "add".to_string()];
    if params.create_branch {
        args.extend(["-b".to_string(), branch.clone()]);
        args.push(target_string.clone());
        args.push(start_from);
    } else {
        args.push(target_string.clone());
        args.push(branch.clone());
    }
    let output = run_git(jail.root(), &args).await?;
    if !output.success {
        return Err(git_error("worktree add", &output));
    }
    let entry = worktree_list(jail)
        .await?
        .entries
        .into_iter()
        .find(|entry| entry.path == target_string)
        .unwrap_or(WorktreeEntry {
            path: target_string,
            head: None,
            branch: Some(format!("refs/heads/{branch}")),
            bare: false,
            detached: false,
            locked: None,
            prunable: None,
        });
    Ok(GitWorktreeAddResult {
        entry,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

pub async fn worktree_list(jail: &PathJail) -> Result<GitWorktreeListResult, RemoteError> {
    let output = run_git(
        jail.root(),
        &[
            "worktree".to_string(),
            "list".to_string(),
            "--porcelain".to_string(),
        ],
    )
    .await?;
    if !output.success {
        return Err(git_error("worktree list", &output));
    }
    let root = jail.root();
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for line in output.stdout.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(entry) = current.take()
                && Path::new(&entry.path).starts_with(root)
            {
                entries.push(entry);
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => {
                current = Some(WorktreeEntry {
                    path: value.to_string(),
                    head: None,
                    branch: None,
                    bare: false,
                    detached: false,
                    locked: None,
                    prunable: None,
                });
            }
            "HEAD" => {
                if let Some(entry) = current.as_mut() {
                    entry.head = Some(value.to_string());
                }
            }
            "branch" => {
                if let Some(entry) = current.as_mut() {
                    entry.branch = Some(value.to_string());
                }
            }
            "bare" => {
                if let Some(entry) = current.as_mut() {
                    entry.bare = true;
                }
            }
            "detached" => {
                if let Some(entry) = current.as_mut() {
                    entry.detached = true;
                }
            }
            "locked" => {
                if let Some(entry) = current.as_mut() {
                    entry.locked = Some(value.to_string());
                }
            }
            "prunable" => {
                if let Some(entry) = current.as_mut() {
                    entry.prunable = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(GitWorktreeListResult { entries })
}

pub async fn worktree_remove(
    jail: &PathJail,
    params: &GitWorktreeRemoveParams,
) -> Result<GitWorktreeRemoveResult, RemoteError> {
    let resolved = jail.resolve_mutation(&params.path)?;
    let path = resolved.absolute.to_string_lossy().to_string();
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if params.force {
        args.push("--force".to_string());
    }
    args.push(path.clone());
    let output = run_git(jail.root(), &args).await?;
    if !output.success {
        return Err(git_error("worktree remove", &output));
    }
    let _ = run_git(jail.root(), &["worktree".to_string(), "prune".to_string()]).await;
    Ok(GitWorktreeRemoveResult {
        path,
        removed: true,
        operation_id: params.operation_id.clone(),
        replayed: false,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command as StdCommand;

    use tempfile::TempDir;

    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo() -> (TempDir, PathJail) {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q"]);
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "first"]);
        let jail = PathJail::new(dir.path()).unwrap();
        (dir, jail)
    }

    #[tokio::test]
    async fn status_reports_clean_and_dirty() {
        let (dir, jail) = repo();
        let clean = status(&jail).await.unwrap();
        assert!(clean.clean);
        assert_eq!(clean.branch.as_deref(), Some("main"));
        fs::write(dir.path().join("b.txt"), "two\n").unwrap();
        let dirty = status(&jail).await.unwrap();
        assert!(!dirty.clean);
        assert!(dirty.entries.iter().any(|entry| entry.path == "b.txt"));
    }

    #[tokio::test]
    async fn diff_and_log_are_bounded() {
        let (dir, jail) = repo();
        fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
        let diff = diff(
            &jail,
            &GitDiffParams {
                scope: DiffScope::Unstaged,
                base: None,
                commit: None,
                paths: Vec::new(),
                context: 3,
                stat_only: false,
            },
        )
        .await
        .unwrap();
        assert!(diff.patch.contains("changed"));
        let log = log(
            &jail,
            &GitLogParams {
                limit: 5,
                paths: Vec::new(),
                since: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].subject, "first");
        let resolved = rev_parse(
            &jail,
            &GitRevParseParams {
                rev: "HEAD".into(),
                commit: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(resolved.value.len(), 40);
        let _ = dir;
    }

    #[tokio::test]
    async fn worktree_lifecycle_stays_inside_root() {
        let (dir, jail) = repo();
        let added = worktree_add(
            &jail,
            &GitWorktreeAddParams {
                path: "worktrees/task".into(),
                branch: "verlinpark/task".into(),
                start_from: Some("HEAD".into()),
                create_branch: true,
                operation_id: Some("op-1".into()),
            },
        )
        .await
        .unwrap();
        assert!(Path::new(&added.entry.path).starts_with(dir.path().canonicalize().unwrap()));
        let list = worktree_list(&jail).await.unwrap();
        assert!(
            list.entries
                .iter()
                .any(|entry| entry.path == added.entry.path)
        );
        let removed = worktree_remove(
            &jail,
            &GitWorktreeRemoveParams {
                path: "worktrees/task".into(),
                force: true,
                operation_id: Some("op-2".into()),
            },
        )
        .await
        .unwrap();
        assert!(removed.removed);
        assert!(!dir.path().join("worktrees/task").exists());
    }

    #[tokio::test]
    async fn worktree_path_escape_is_rejected() {
        let (_dir, jail) = repo();
        let error = worktree_add(
            &jail,
            &GitWorktreeAddParams {
                path: "../escape".into(),
                branch: "verlinpark/escape".into(),
                start_from: None,
                create_branch: true,
                operation_id: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PathEscape);
    }

    #[tokio::test]
    async fn unsafe_branch_names_are_rejected() {
        assert!(validate_branch("--force").is_err());
        assert!(validate_branch("a..b").is_err());
        assert!(validate_branch("a b").is_err());
        assert!(validate_branch("feature/task-1").is_ok());
        assert!(validate_revision("HEAD~1", "rev").is_ok());
        assert!(validate_revision("HEAD^2", "rev").is_ok());
        assert!(validate_revision("--upload-pack=evil", "rev").is_err());
    }

    #[test]
    fn status_parser_handles_renames() {
        let entries = parse_status_entries("R  new.txt\0old.txt\0?? x\0");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].original_path.as_deref(), Some("old.txt"));
        assert_eq!(entries[1].path, "x");
    }
}

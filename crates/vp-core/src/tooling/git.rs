//! Read-only structured Git queries.
//!
//! The runtime owns commits, branch movement, staging, and integration. Agents
//! only need to inspect repository state, so this tool deliberately has no
//! write actions. Returned data is bounded and parsed; the model does not have
//! to scrape `git status` or `git log` output.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::{AgentTool, ToolContext, ToolOutput};

const MAX_GIT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PATCH_CHARS: usize = 60_000;
const MAX_STATUS_ENTRIES: usize = 500;
const MAX_LOG_ENTRIES: usize = 100;
const MAX_FILES: usize = 2_000;
const MAX_BLAME_LINES: usize = 1_000;

pub struct GitQuery;

#[async_trait]
impl AgentTool for GitQuery {
    fn name(&self) -> &'static str {
        "git"
    }

    fn description(&self) -> &'static str {
        "Read repository state without mutating it: status, diff, log, show, blame, or tracked files. Commits and integration are runtime-owned."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["status", "diff", "log", "show", "blame", "files"]},
                "scope": {"type": "string", "enum": ["unstaged", "staged", "base", "commit"], "description": "Diff scope; default unstaged"},
                "base": {"type": "string", "description": "Base ref for scope=base; default HEAD"},
                "commit": {"type": "string", "description": "Commit for show/files/blame; default HEAD"},
                "path": {"type": "string", "description": "Single path filter (blame requires it)"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Path filters for diff/log/show"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Log entry cap; default 20"},
                "context": {"type": "integer", "minimum": 0, "maximum": 20, "description": "Diff context lines; default 3"},
                "stat_only": {"type": "boolean", "description": "Return diffstat without the patch"},
                "since": {"type": "string", "description": "Log --since value, e.g. 2.weeks"},
                "start_line": {"type": "integer", "minimum": 1, "description": "Blame start line"},
                "end_line": {"type": "integer", "minimum": 1, "description": "Blame end line"}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        let paths = input
            .get("paths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match action {
            "status" => git_status(context),
            "diff" => git_diff(context, &input, &paths),
            "log" => git_log(context, &input, &paths),
            "show" => git_show(context, &input, &paths),
            "blame" => git_blame(context, &input),
            "files" => git_files(context, &input),
            _ => Ok(ToolOutput::error_structured(
                format!("unsupported git action: {action}"),
                json!({"code": "INVALID_ARGUMENT", "action": action}),
            )),
        }
    }
}

fn git_status(context: &ToolContext) -> Result<ToolOutput> {
    let branch = run_git(&context.root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|output| output.stdout.trim().to_string());
    let head = run_git(&context.root, &["rev-parse", "HEAD"])
        .ok()
        .map(|output| output.stdout.trim().to_string());
    let upstream = run_git(
        &context.root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .ok()
    .map(|output| output.stdout.trim().to_string())
    .filter(|value| !value.is_empty());
    let (ahead, behind) = upstream
        .as_ref()
        .and_then(|upstream| {
            run_git(
                &context.root,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("HEAD...{upstream}"),
                ],
            )
            .ok()
        })
        .and_then(|output| {
            let mut parts = output.stdout.split_whitespace();
            let ahead = parts.next()?.parse::<u64>().ok()?;
            let behind = parts.next()?.parse::<u64>().ok()?;
            Some((ahead, behind))
        })
        .unwrap_or((0, 0));
    let status = run_git(&context.root, &["status", "--porcelain=v1", "-z"])?;
    let entries = parse_status(&status.stdout);
    let clean = entries.is_empty();
    let rendered = entries
        .iter()
        .take(40)
        .map(|entry| {
            format!(
                "{} {}",
                entry["status"].as_str().unwrap_or("??"),
                entry["path"].as_str().unwrap_or("?")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let summary = format!(
        "{} {} · {} change(s){}{}",
        branch.as_deref().unwrap_or("detached"),
        head.as_deref()
            .map(|hash| &hash[..8.min(hash.len())])
            .unwrap_or("-"),
        entries.len(),
        if ahead > 0 {
            format!(" · ahead {ahead}")
        } else {
            String::new()
        },
        if behind > 0 {
            format!(" · behind {behind}")
        } else {
            String::new()
        }
    );
    Ok(ToolOutput::ok_structured_preserving(
        format!("{summary}\n{rendered}"),
        json!({
            "action": "status",
            "branch": branch,
            "head": head,
            "upstream": upstream,
            "ahead": ahead,
            "behind": behind,
            "clean": clean,
            "changes": entries,
            "truncated": status.truncated || entries.len() >= MAX_STATUS_ENTRIES,
            "worktree": context.root.to_string_lossy().to_string(),
        }),
        false,
    ))
}

fn parse_status(text: &str) -> Vec<Value> {
    let mut entries = Vec::new();
    let mut fields = text.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let x = field.as_bytes()[0] as char;
        let y = field.as_bytes()[1] as char;
        let path = field[3..].to_string();
        let original_path = if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            fields.next().map(str::to_string)
        } else {
            None
        };
        let status = if x == '?' && y == '?' {
            "untracked"
        } else if matches!(x, 'U' | 'A') && matches!(y, 'U' | 'A' | 'D')
            || matches!(x, 'D') && matches!(y, 'U' | 'A')
        {
            "conflict"
        } else {
            "modified"
        };
        entries.push(json!({
            "path": path,
            "original_path": original_path,
            "index": x.to_string(),
            "worktree": y.to_string(),
            "status": status,
        }));
        if entries.len() >= MAX_STATUS_ENTRIES {
            break;
        }
    }
    entries
}

fn git_diff(context: &ToolContext, input: &Value, paths: &[String]) -> Result<ToolOutput> {
    let scope = input
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("unstaged");
    let context_lines = input
        .get("context")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .min(20);
    let stat_only = input
        .get("stat_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let base = input.get("base").and_then(Value::as_str).unwrap_or("HEAD");
    let commit = input
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    let mut args = vec!["diff".to_string()];
    match scope {
        "staged" => args.push("--cached".to_string()),
        "base" => {
            validate_ref(base)?;
            args.push(base.to_string());
        }
        "commit" => {
            validate_ref(commit)?;
            args = vec![
                "show".to_string(),
                "--format=".to_string(),
                commit.to_string(),
            ];
        }
        _ => {}
    }
    if !args.iter().any(|arg| arg == "--format=") {
        args.push(format!("--unified={context_lines}"));
    }
    args.push("--no-color".to_string());
    args.push("--no-ext-diff".to_string());
    if stat_only {
        args.push("--stat".to_string());
    }
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths.iter().cloned());
    }
    let patch = run_git(&context.root, &args)?;
    let mut numstat_args = if scope == "commit" {
        vec![
            "show".to_string(),
            "--format=".to_string(),
            "--numstat".to_string(),
            commit.to_string(),
        ]
    } else {
        let mut args = vec!["diff".to_string()];
        if scope == "staged" {
            args.push("--cached".to_string());
        } else if scope == "base" {
            args.push(base.to_string());
        }
        args.push("--numstat".to_string());
        args
    };
    if !paths.is_empty() {
        numstat_args.push("--".to_string());
        numstat_args.extend(paths.iter().cloned());
    }
    let numstat = run_git(&context.root, &numstat_args).ok();
    let files = numstat
        .map(|output| {
            output
                .stdout
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(3, '\t');
                    let additions = parts.next()?;
                    let deletions = parts.next()?;
                    let path = parts.next()?;
                    Some(json!({
                        "path": path,
                        "additions": additions.parse::<u64>().ok(),
                        "deletions": deletions.parse::<u64>().ok(),
                        "binary": additions == "-",
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let patch_text = truncate_chars(&patch.stdout, MAX_PATCH_CHARS);
    let summary = format!(
        "{} · {} file(s), {} patch chars{}",
        scope,
        files.len(),
        patch_text.len(),
        if patch.truncated { " (capped)" } else { "" }
    );
    Ok(ToolOutput::ok_structured_preserving(
        format!("{summary}\n{patch_text}"),
        json!({
            "action": "diff",
            "scope": scope,
            "files": files,
            "patch": patch_text,
            "stat_only": stat_only,
            "truncated": patch.truncated || patch.stdout.len() > MAX_PATCH_CHARS,
        }),
        false,
    ))
}

fn git_log(context: &ToolContext, input: &Value, paths: &[String]) -> Result<ToolOutput> {
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, MAX_LOG_ENTRIES as u64);
    let since = input.get("since").and_then(Value::as_str);
    let format = "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%s%x1e";
    let mut args = vec![
        "log".to_string(),
        "--no-color".to_string(),
        "--date=iso-strict".to_string(),
        format.to_string(),
        "--name-only".to_string(),
        format!("-n{limit}"),
    ];
    if let Some(since) = since {
        validate_ref(since)?;
        args.push(format!("--since={since}"));
    }
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths.iter().cloned());
    }
    let output = run_git(&context.root, &args)?;
    let commits = output
        .stdout
        .split('\x1e')
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let mut lines = record.lines().filter(|line| !line.trim().is_empty());
            let fields = lines
                .next()
                .unwrap_or_default()
                .split('\x1f')
                .collect::<Vec<_>>();
            let files = lines.map(str::to_string).take(40).collect::<Vec<_>>();
            json!({
                "hash": fields.first().copied().unwrap_or_default(),
                "short_hash": fields.get(1).copied().unwrap_or_default(),
                "author": fields.get(2).copied().unwrap_or_default(),
                "email": fields.get(3).copied().unwrap_or_default(),
                "date": fields.get(4).copied().unwrap_or_default(),
                "subject": fields.get(5).copied().unwrap_or_default(),
                "files": files,
            })
        })
        .collect::<Vec<_>>();
    let rendered = commits
        .iter()
        .map(|commit| {
            format!(
                "{} {} ({})",
                commit["short_hash"].as_str().unwrap_or("?"),
                commit["subject"].as_str().unwrap_or(""),
                commit["date"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok_structured_preserving(
        rendered,
        json!({
            "action": "log",
            "commits": commits,
            "truncated": output.truncated,
        }),
        false,
    ))
}

fn git_show(context: &ToolContext, input: &Value, paths: &[String]) -> Result<ToolOutput> {
    let commit = input
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    validate_ref(commit)?;
    let mut stat_args = vec![
        "show".to_string(),
        "--no-color".to_string(),
        "--date=iso-strict".to_string(),
        "--format=fuller".to_string(),
        "--stat".to_string(),
        commit.to_string(),
    ];
    if !paths.is_empty() {
        stat_args.push("--".to_string());
        stat_args.extend(paths.iter().cloned());
    }
    let stat = run_git(&context.root, &stat_args)?;
    let mut patch_args = vec![
        "show".to_string(),
        "--no-color".to_string(),
        "--format=".to_string(),
        commit.to_string(),
    ];
    if !paths.is_empty() {
        patch_args.push("--".to_string());
        patch_args.extend(paths.iter().cloned());
    }
    let patch = run_git(&context.root, &patch_args)?;
    let patch_text = truncate_chars(&patch.stdout, MAX_PATCH_CHARS);
    Ok(ToolOutput::ok_structured_preserving(
        format!("{}\n{patch_text}", stat.stdout.trim_end()),
        json!({
            "action": "show",
            "commit": commit,
            "stat": stat.stdout,
            "patch": patch_text,
            "truncated": stat.truncated || patch.truncated,
        }),
        false,
    ))
}

fn git_blame(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("path is required for blame"))?;
    let start = input
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    let end = input
        .get("end_line")
        .and_then(Value::as_u64)
        .unwrap_or(start.saturating_add(99))
        .max(start);
    if end.saturating_sub(start) > MAX_BLAME_LINES as u64 {
        return Ok(ToolOutput::error_structured(
            format!("blame range is limited to {MAX_BLAME_LINES} lines"),
            json!({"code": "INVALID_ARGUMENT", "start_line": start, "end_line": end}),
        ));
    }
    let commit = input
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    validate_ref(commit)?;
    let mut args = vec![
        "blame".to_string(),
        "--line-porcelain".to_string(),
        format!("-L{start},{end}"),
        commit.to_string(),
    ];
    args.push("--".to_string());
    args.push(path.to_string());
    let output = run_git(&context.root, &args)?;
    let mut entries = Vec::new();
    let mut current = json!({});
    for line in output.stdout.lines() {
        if line.starts_with('\t') {
            current["line"] = json!(current["final_line"].as_u64().unwrap_or(0));
            current["content"] = json!(line.trim_start_matches('\t'));
            entries.push(current.clone());
            current = json!({});
            continue;
        }
        if line.starts_with("author ") {
            current["author"] = json!(line.trim_start_matches("author "));
        } else if line.starts_with("author-mail ") {
            current["email"] = json!(
                line.trim_start_matches("author-mail ")
                    .trim_matches(['<', '>'])
            );
        } else if line.starts_with("author-time ") {
            current["author_time"] = json!(line.trim_start_matches("author-time "));
        } else if line.starts_with("summary ") {
            current["summary"] = json!(line.trim_start_matches("summary "));
        } else if !line.starts_with("previous ")
            && !line.starts_with("filename ")
            && !line.starts_with("boundary")
        {
            let mut fields = line.split_whitespace();
            if let Some(hash) = fields.next()
                && fields.next().is_some()
                && let Some(final_line) = fields.next()
                && let Ok(final_line) = final_line.parse::<u64>()
            {
                current["commit"] = json!(hash);
                current["final_line"] = json!(final_line);
            }
        }
    }
    let rendered = entries
        .iter()
        .map(|entry| {
            format!(
                "{:>5} {} {}",
                entry["line"].as_u64().unwrap_or(0),
                entry["author"].as_str().unwrap_or("?"),
                entry["content"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::ok_structured_preserving(
        rendered,
        json!({
            "action": "blame",
            "path": path,
            "lines": entries,
            "truncated": output.truncated,
        }),
        false,
    ))
}

fn git_files(context: &ToolContext, input: &Value) -> Result<ToolOutput> {
    let commit = input.get("commit").and_then(Value::as_str);
    let args = if let Some(commit) = commit {
        validate_ref(commit)?;
        vec![
            "ls-tree".to_string(),
            "-r".to_string(),
            "--name-only".to_string(),
            commit.to_string(),
        ]
    } else {
        vec!["ls-files".to_string()]
    };
    let output = run_git(&context.root, &args)?;
    let files = output
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .take(MAX_FILES)
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok(ToolOutput::ok_structured_preserving(
        files.join("\n"),
        json!({
            "action": "files",
            "commit": commit,
            "files": files,
            "truncated": output.truncated || files.len() >= MAX_FILES,
        }),
        false,
    ))
}

fn validate_ref(value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("Git ref must not be empty"));
    }
    if value.starts_with('-')
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || value.contains("..")
    {
        return Err(anyhow!("invalid Git ref: {value}"));
    }
    Ok(())
}

struct GitOutput {
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
    #[allow(dead_code)]
    exit_code: i32,
    truncated: bool,
}

fn run_git<S: AsRef<str>>(root: &Path, args: &[S]) -> Result<GitOutput> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("--no-pager")
        .args(["-c", "color.ui=false"])
        .args(args.iter().map(AsRef::as_ref))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning git; is Git installed?")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("git stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("git stderr pipe is unavailable"))?;
    let (stdout, stdout_truncated) = read_capped(stdout)?;
    let (stderr, _) = read_capped(stderr)?;
    let status = child.wait()?;
    let exit_code = status.code().unwrap_or(-1);
    if !status.success() && exit_code != 1 {
        return Err(anyhow!(
            "git {} failed: {}",
            args.first().map(AsRef::as_ref).unwrap_or("?"),
            stderr.trim()
        ));
    }
    Ok(GitOutput {
        stdout,
        stderr,
        exit_code,
        truncated: stdout_truncated,
    })
}

fn read_capped<R: Read>(mut reader: R) -> Result<(String, bool)> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_GIT_OUTPUT_BYTES.saturating_sub(bytes.len());
        if remaining > 0 {
            bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        if read > remaining {
            truncated = true;
        }
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut output = text.chars().take(limit).collect::<String>();
    output.push_str("\n… patch truncated");
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: root.join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: None,
            process_supervisor: None,
        }
    }

    fn init_repo(root: &Path) {
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "initial"]);
    }

    #[tokio::test]
    async fn queries_status_diff_log_files_and_blame() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let context = context(dir.path());

        let status = GitQuery
            .execute(&context, json!({"action": "status"}))
            .await
            .unwrap();
        assert!(!status.is_error, "{}", status.content);
        assert_eq!(status.structured.unwrap()["clean"], true);

        std::fs::write(dir.path().join("a.txt"), "one\nTWO\n").unwrap();
        let status = GitQuery
            .execute(&context, json!({"action": "status"}))
            .await
            .unwrap();
        let structured = status.structured.unwrap();
        assert_eq!(structured["clean"], false);
        assert_eq!(structured["changes"].as_array().unwrap().len(), 1);

        let diff = GitQuery
            .execute(&context, json!({"action": "diff"}))
            .await
            .unwrap();
        assert!(!diff.is_error);
        assert!(diff.content.contains("+TWO"));

        let log = GitQuery
            .execute(&context, json!({"action": "log", "limit": 5}))
            .await
            .unwrap();
        assert!(log.content.contains("initial"));

        let files = GitQuery
            .execute(&context, json!({"action": "files"}))
            .await
            .unwrap();
        assert!(files.content.contains("a.txt"));

        let blame = GitQuery
            .execute(
                &context,
                json!({"action": "blame", "path": "a.txt", "start_line": 1, "end_line": 2}),
            )
            .await
            .unwrap();
        assert!(!blame.is_error, "{}", blame.content);
        assert!(blame.content.contains("Test"));
    }

    #[tokio::test]
    async fn rejects_option_injection_in_refs() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let output = GitQuery
            .execute(
                &context(dir.path()),
                json!({"action": "show", "commit": "--help"}),
            )
            .await;
        assert!(output.is_err());
    }
}

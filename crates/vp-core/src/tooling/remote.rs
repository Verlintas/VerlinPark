//! Explicit remote-workspace tools.
//!
//! These tools are only useful when the session's project has an `ssh`
//! execution host. They do not replace or reroute `read_file`, `write_file`,
//! `shell`, or `git`: a local project cannot call them, and a remote project
//! still has its local tools for its session worktree. All paths are relative
//! to the configured remote root; an absolute path or `..` is rejected before a
//! connection is opened.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::remote::RemoteToolAccess;
use crate::tools::{AgentTool, ToolContext, ToolOutput, truncate};
use vp_remote::protocol::{
    DiffScope, ExecParams, GitDiffParams, GitLogParams, GitRevParseParams, GitWorktreeAddParams,
    GitWorktreeRemoveParams, ListDirParams, ReadEncoding, ReadFileParams, WriteEncoding,
    WriteFileParams,
};

const DEFAULT_READ_LIMIT: u64 = 64 * 1024;
const MAX_READ_LIMIT: u64 = 256 * 1024;
const DEFAULT_LIST_LIMIT: u32 = 200;
const DEFAULT_EXEC_CAP: u64 = 64 * 1024;
const MAX_EXEC_CAP: u64 = 4 * 1024 * 1024;
const MAX_WRITE_CONTENT_CHARS: usize = 200_000;

fn access<'a>(context: &'a ToolContext, input: &Value) -> Result<&'a RemoteToolAccess> {
    let requested = input.get("project_id").and_then(Value::as_str);
    let Some(access) = context.remote.as_deref() else {
        bail!(
            "this project has no ssh execution host; remote tools are unavailable. Use the local file/shell/git tools for the session worktree"
        );
    };
    access.ensure_project(requested)?;
    Ok(access)
}

fn operation_id(input: &Value, tool: &str) -> String {
    input
        .get("operation_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{tool}:{}", uuid::Uuid::now_v7()))
}

fn project_id_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional project id; must match the session project"
    })
}

fn operation_id_schema() -> Value {
    json!({
        "type": "string",
        "maxLength": 160,
        "description": "Stable idempotency key for retries of this exact mutation"
    })
}

pub struct RemoteRead;

#[async_trait]
impl AgentTool for RemoteRead {
    fn name(&self) -> &'static str {
        "remote_read"
    }

    fn description(&self) -> &'static str {
        "Read a file from the project's configured remote SSH workspace. Paths are relative to the remote root; content is UTF-8 or base64 with a SHA-256 for later writes."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the configured remote root"},
                "offset": {"type": "integer", "minimum": 0, "description": "Zero-based byte offset"},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_READ_LIMIT, "description": "Maximum bytes; default 64 KiB"},
                "encoding": {"type": "string", "enum": ["auto", "utf8", "base64"], "description": "Default auto"},
                "project_id": project_id_schema()
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let access = access(context, &input)?;
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let path = access.validate_path(path)?;
        let encoding = match input
            .get("encoding")
            .and_then(Value::as_str)
            .unwrap_or("auto")
        {
            "utf8" => ReadEncoding::Utf8,
            "base64" => ReadEncoding::Base64,
            _ => ReadEncoding::Auto,
        };
        let params = ReadFileParams {
            path,
            offset: input.get("offset").and_then(Value::as_u64).unwrap_or(0),
            limit: input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_READ_LIMIT)
                .clamp(1, MAX_READ_LIMIT),
            encoding,
        };
        let workspace = access.workspace().await?;
        let result = workspace.read_file(params).await?;
        let header = format!(
            "remote {} {} ({} bytes, sha256 {}, {}, offset {}, {} bytes read{} )",
            access.label(),
            result.path,
            result.size,
            result.sha256,
            match result.encoding {
                ReadEncoding::Utf8 => "utf-8",
                ReadEncoding::Base64 => "base64",
                ReadEncoding::Auto => "auto",
            },
            result.offset,
            result.bytes_read,
            if result.truncated { ", truncated" } else { "" }
        );
        let body = if let Some(text) = result.text.as_ref() {
            text.clone()
        } else if let Some(base64) = result.base64.as_ref() {
            format!("<base64>{base64}</base64>")
        } else {
            String::new()
        };
        Ok(ToolOutput::ok_structured(
            truncate(format!("{header}\n{body}")),
            json!({
                "remote": true,
                "path": result.path,
                "sha256": result.sha256,
                "size": result.size,
                "offset": result.offset,
                "bytes_read": result.bytes_read,
                "encoding": match result.encoding {
                    ReadEncoding::Utf8 => "utf8",
                    ReadEncoding::Base64 => "base64",
                    ReadEncoding::Auto => "auto",
                },
                "truncated": result.truncated,
                "text": result.text,
                "base64": result.base64,
            }),
        ))
    }
}

pub struct RemoteWrite;

#[async_trait]
impl AgentTool for RemoteWrite {
    fn name(&self) -> &'static str {
        "remote_write"
    }

    fn description(&self) -> &'static str {
        "Atomically write a file in the project's remote SSH workspace. Existing files require expected_sha256 (or allow_overwrite=true); content is written to a temp file and renamed."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the configured remote root"},
                "content": {"type": "string", "maxLength": MAX_WRITE_CONTENT_CHARS},
                "encoding": {"type": "string", "enum": ["utf8", "base64"], "description": "Default utf8"},
                "expected_sha256": {"type": "string", "description": "SHA-256 returned by remote_read; required to replace an existing file"},
                "create_parents": {"type": "boolean", "description": "Create missing parent directories; default true"},
                "allow_overwrite": {"type": "boolean", "description": "Explicitly replace without an expected hash; default false"},
                "mode": {"type": "integer", "minimum": 0, "maximum": 511, "description": "Optional POSIX mode"},
                "project_id": project_id_schema(),
                "operation_id": operation_id_schema()
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let access = access(context, &input)?;
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let path = access.validate_path(path)?;
        let content = input
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("content is required"))?;
        let params = WriteFileParams {
            path,
            content: content.to_string(),
            encoding: match input.get("encoding").and_then(Value::as_str) {
                Some("base64") => WriteEncoding::Base64,
                _ => WriteEncoding::Utf8,
            },
            expected_sha256: input
                .get("expected_sha256")
                .and_then(Value::as_str)
                .map(str::to_string),
            create_parents: input
                .get("create_parents")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            allow_overwrite: input
                .get("allow_overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            mode: input
                .get("mode")
                .and_then(Value::as_u64)
                .map(|mode| mode as u32),
            operation_id: Some(operation_id(&input, "remote_write")),
        };
        let workspace = access.workspace().await?;
        let result = workspace.write_file(params).await?;
        Ok(ToolOutput::ok_structured(
            format!(
                "remote {} {}: {} {} bytes, sha256 {}{}",
                access.label(),
                result.path,
                if result.created {
                    "created"
                } else {
                    "replaced"
                },
                result.size,
                result.sha256,
                if result.replayed { " (replayed)" } else { "" }
            ),
            json!({
                "remote": true,
                "path": result.path,
                "sha256": result.sha256,
                "size": result.size,
                "created": result.created,
                "replaced": result.replaced,
                "replayed": result.replayed,
            }),
        ))
    }
}

pub struct RemoteList;

#[async_trait]
impl AgentTool for RemoteList {
    fn name(&self) -> &'static str {
        "remote_list"
    }

    fn description(&self) -> &'static str {
        "List one directory in the project's remote SSH workspace with hidden-file filtering and pagination."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the remote root; default ."},
                "include_hidden": {"type": "boolean", "description": "Include dotfiles; default true"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Default 200"},
                "cursor": {"type": "integer", "minimum": 0, "description": "Zero-based entry offset"},
                "project_id": project_id_schema()
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let access = access(context, &input)?;
        let path = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let params = ListDirParams {
            path: access.validate_path(path)?,
            include_hidden: input
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            limit: input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_LIST_LIMIT as u64)
                .clamp(1, 2000) as u32,
            cursor: input.get("cursor").and_then(Value::as_u64).unwrap_or(0) as u32,
        };
        let workspace = access.workspace().await?;
        let result = workspace.list_dir(params).await?;
        let lines = result
            .entries
            .iter()
            .map(|entry| {
                format!(
                    "{} {} {}",
                    match entry.kind {
                        vp_remote::EntryKind::Directory => "d",
                        vp_remote::EntryKind::File => "f",
                        vp_remote::EntryKind::Symlink => "l",
                        vp_remote::EntryKind::Other => "?",
                    },
                    entry.path,
                    if entry.kind == vp_remote::EntryKind::File {
                        format!("({} bytes)", entry.size)
                    } else {
                        String::new()
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut text = format!(
            "remote {} {}: {} entries\n{}",
            access.label(),
            result.path,
            result.total,
            lines
        );
        if let Some(cursor) = result.next_cursor {
            text.push_str(&format!("\n… next_cursor={cursor}"));
        }
        Ok(ToolOutput::ok_structured(
            truncate(text),
            json!({
                "remote": true,
                "path": result.path,
                "entries": result.entries,
                "total": result.total,
                "next_cursor": result.next_cursor,
                "truncated": result.truncated,
            }),
        ))
    }
}

pub struct RemoteExec;

#[async_trait]
impl AgentTool for RemoteExec {
    fn name(&self) -> &'static str {
        "remote_exec"
    }

    fn description(&self) -> &'static str {
        "Run a shell command on the project's configured remote host under an allowlisted environment, bounded timeout/output, and process-group cancellation. Requires approval and respects the project ShellPolicy."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "maxLength": 131072},
                "cwd": {"type": "string", "description": "Directory relative to the remote root; default root"},
                "env": {
                    "type": "object",
                    "maxProperties": 32,
                    "additionalProperties": {"type": "string", "maxLength": 8192},
                    "description": "Only helper-allowlisted names are accepted"
                },
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "Default 120"},
                "output_cap_bytes": {"type": "integer", "minimum": 1024, "maximum": MAX_EXEC_CAP, "description": "Per-stream cap; default 64 KiB"},
                "project_id": project_id_schema(),
                "operation_id": operation_id_schema()
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let access = access(context, &input)?;
        access.ensure_exec_allowed()?;
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("command is required"))?;
        if command.trim().is_empty() {
            bail!("command must not be empty");
        }
        let cwd = input
            .get("cwd")
            .and_then(Value::as_str)
            .map(|path| access.validate_path(path))
            .transpose()?;
        let env = input
            .get("env")
            .and_then(Value::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| {
                        value
                            .as_str()
                            .map(|value| (key.clone(), value.to_string()))
                            .ok_or_else(|| anyhow!("remote_exec env values must be strings"))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()
            })
            .transpose()?
            .unwrap_or_default();
        let params = ExecParams {
            command: command.to_string(),
            cwd,
            env,
            timeout_ms: input
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(120)
                .clamp(1, 3600)
                * 1000,
            output_cap_bytes: input
                .get("output_cap_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_EXEC_CAP)
                .clamp(1024, MAX_EXEC_CAP),
            operation_id: Some(operation_id(&input, "remote_exec")),
        };
        let workspace = access.workspace().await?;
        let result = workspace.exec(params).await?;
        let mut text = format!(
            "remote {} exit={} duration={}ms{}{}{}\n",
            access.label(),
            result
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            result.duration_ms,
            if result.timed_out { " timed_out" } else { "" },
            if result.cancelled { " cancelled" } else { "" },
            if result.replayed { " replayed" } else { "" }
        );
        if !result.stdout.is_empty() {
            text.push_str("--- stdout ---\n");
            text.push_str(&result.stdout);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
        if !result.stderr.is_empty() {
            text.push_str("--- stderr ---\n");
            text.push_str(&result.stderr);
        }
        Ok(ToolOutput::ok_structured_preserving(
            text,
            json!({
                "remote": true,
                "exit_code": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "stdout_truncated": result.stdout_truncated,
                "stderr_truncated": result.stderr_truncated,
                "timed_out": result.timed_out,
                "cancelled": result.cancelled,
                "duration_ms": result.duration_ms,
                "replayed": result.replayed,
            }),
            false,
        ))
    }
}

pub struct RemoteGit;

#[async_trait]
impl AgentTool for RemoteGit {
    fn name(&self) -> &'static str {
        "remote_git"
    }

    fn description(&self) -> &'static str {
        "Read remote repository state through structured Git operations: status, diff, log, or rev_parse. Worktree mutations are on remote_worktree."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["status", "diff", "log", "rev_parse"]},
                "scope": {"type": "string", "enum": ["unstaged", "staged", "base", "commit"], "description": "Diff scope; default unstaged"},
                "base": {"type": "string"},
                "commit": {"type": "string"},
                "paths": {"type": "array", "maxItems": 100, "items": {"type": "string"}},
                "context": {"type": "integer", "minimum": 0, "maximum": 20},
                "stat_only": {"type": "boolean"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                "since": {"type": "string"},
                "rev": {"type": "string", "description": "Revision for rev_parse; default HEAD"},
                "project_id": project_id_schema()
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let access = access(context, &input)?;
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        let workspace = access.workspace().await?;
        match action {
            "status" => {
                let result = workspace.git_status().await?;
                let mut text = format!(
                    "remote {} branch={} head={}{}\n",
                    access.label(),
                    result.branch.as_deref().unwrap_or("(detached)"),
                    result
                        .head
                        .as_deref()
                        .map(|head| &head[..head.len().min(12)])
                        .unwrap_or("?"),
                    if result.clean { " clean" } else { " dirty" }
                );
                for entry in result.entries.iter().take(100) {
                    text.push_str(&format!("{} {}\n", entry.status, entry.path));
                }
                Ok(ToolOutput::ok_structured(
                    truncate(text),
                    json!({
                        "remote": true,
                        "branch": result.branch,
                        "head": result.head,
                        "upstream": result.upstream,
                        "ahead": result.ahead,
                        "behind": result.behind,
                        "clean": result.clean,
                        "entries": result.entries,
                    }),
                ))
            }
            "diff" => {
                let paths = input
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|paths| {
                        paths
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|path| access.validate_path(path))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                let scope = match input.get("scope").and_then(Value::as_str) {
                    Some("staged") => DiffScope::Staged,
                    Some("base") => DiffScope::Base,
                    Some("commit") => DiffScope::Commit,
                    _ => DiffScope::Unstaged,
                };
                let result = workspace
                    .git_diff(GitDiffParams {
                        scope,
                        base: input
                            .get("base")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        commit: input
                            .get("commit")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        paths,
                        context: input
                            .get("context")
                            .and_then(Value::as_u64)
                            .unwrap_or(3)
                            .min(20) as u32,
                        stat_only: input
                            .get("stat_only")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                    .await?;
                let mut text = result.patch.clone();
                if text.is_empty() {
                    text = result.stat.clone();
                }
                Ok(ToolOutput::ok_structured(
                    truncate(format!("remote {} diff\n{text}", access.label())),
                    json!({
                        "remote": true,
                        "scope": format!("{scope:?}").to_ascii_lowercase(),
                        "patch": result.patch,
                        "stat": result.stat,
                        "truncated": result.truncated,
                    }),
                ))
            }
            "log" => {
                let paths = input
                    .get("paths")
                    .and_then(Value::as_array)
                    .map(|paths| {
                        paths
                            .iter()
                            .filter_map(Value::as_str)
                            .map(|path| access.validate_path(path))
                            .collect::<Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                let result = workspace
                    .git_log(GitLogParams {
                        limit: input
                            .get("limit")
                            .and_then(Value::as_u64)
                            .unwrap_or(20)
                            .clamp(1, 200) as u32,
                        paths,
                        since: input
                            .get("since")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                    .await?;
                let text = result
                    .entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "{} {} {} <{}> {}",
                            entry.short_hash,
                            entry.timestamp,
                            entry.author,
                            entry.email,
                            entry.subject
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolOutput::ok_structured(
                    truncate(format!("remote {} log\n{text}", access.label())),
                    json!({
                        "remote": true,
                        "entries": result.entries,
                        "truncated": result.truncated,
                    }),
                ))
            }
            "rev_parse" => {
                let result = workspace
                    .git_rev_parse(GitRevParseParams {
                        rev: input
                            .get("rev")
                            .and_then(Value::as_str)
                            .unwrap_or("HEAD")
                            .to_string(),
                        commit: true,
                    })
                    .await?;
                Ok(ToolOutput::ok_structured(
                    format!(
                        "remote {} {} -> {}",
                        access.label(),
                        result.rev,
                        result.value
                    ),
                    json!({"remote": true, "rev": result.rev, "value": result.value}),
                ))
            }
            other => Ok(ToolOutput::error_structured(
                format!("unsupported remote_git action: {other}"),
                json!({"code": "INVALID_ARGUMENT", "action": other}),
            )),
        }
    }
}

pub struct RemoteWorktree;

#[async_trait]
impl AgentTool for RemoteWorktree {
    fn name(&self) -> &'static str {
        "remote_worktree"
    }

    fn description(&self) -> &'static str {
        "List, add, or remove Git worktrees on the project's remote host. Worktree paths are root-relative and start_from defaults to HEAD."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "add", "remove"]},
                "path": {"type": "string", "description": "Root-relative worktree path"},
                "branch": {"type": "string", "description": "Branch to create or attach"},
                "start_from": {"type": "string", "description": "Commit-ish for a new branch; default HEAD"},
                "create_branch": {"type": "boolean", "description": "Default true"},
                "force": {"type": "boolean", "description": "Force removal of a dirty worktree"},
                "project_id": project_id_schema(),
                "operation_id": operation_id_schema()
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let access = access(context, &input)?;
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        let workspace = access.workspace().await?;
        match action {
            "list" => {
                let worktrees = workspace.worktree_list().await?;
                let text = worktrees
                    .iter()
                    .map(|worktree| {
                        format!(
                            "{} {} {}{}",
                            worktree.path,
                            worktree.branch.as_deref().unwrap_or("(detached)"),
                            worktree.head.as_deref().unwrap_or("?"),
                            if worktree.locked.is_some() {
                                " locked"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolOutput::ok_structured(
                    truncate(format!("remote {} worktrees\n{text}", access.label())),
                    json!({"remote": true, "worktrees": worktrees}),
                ))
            }
            "add" => {
                let path = input
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("path is required for worktree add"))?;
                let branch = input
                    .get("branch")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("branch is required for worktree add"))?;
                let result = workspace
                    .worktree_add(GitWorktreeAddParams {
                        path: access.validate_path(path)?,
                        branch: branch.to_string(),
                        start_from: input
                            .get("start_from")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        create_branch: input
                            .get("create_branch")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        operation_id: Some(operation_id(&input, "remote_worktree_add")),
                    })
                    .await?;
                Ok(ToolOutput::ok_structured(
                    format!(
                        "remote {} worktree added: {} ({}){}",
                        access.label(),
                        result.entry.path,
                        result.entry.branch.as_deref().unwrap_or("detached"),
                        if result.replayed { " (replayed)" } else { "" }
                    ),
                    json!({
                        "remote": true,
                        "worktree": result.entry,
                        "replayed": result.replayed,
                    }),
                ))
            }
            "remove" => {
                let path = input
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("path is required for worktree remove"))?;
                let result = workspace
                    .worktree_remove(GitWorktreeRemoveParams {
                        path: access.validate_path(path)?,
                        force: input.get("force").and_then(Value::as_bool).unwrap_or(false),
                        operation_id: Some(operation_id(&input, "remote_worktree_remove")),
                    })
                    .await?;
                Ok(ToolOutput::ok_structured(
                    format!(
                        "remote {} worktree removed: {}{}",
                        access.label(),
                        result.path,
                        if result.replayed { " (replayed)" } else { "" }
                    ),
                    json!({
                        "remote": true,
                        "path": result.path,
                        "removed": result.removed,
                        "replayed": result.replayed,
                    }),
                ))
            }
            other => Ok(ToolOutput::error_structured(
                format!("unsupported remote_worktree action: {other}"),
                json!({"code": "INVALID_ARGUMENT", "action": other}),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_are_generated_and_preserved() {
        let generated = operation_id(&json!({}), "remote_write");
        assert!(generated.starts_with("remote_write:"));
        assert_eq!(
            operation_id(&json!({"operation_id": "stable"}), "remote_write"),
            "stable"
        );
    }

    #[test]
    fn schemas_are_bounded() {
        let schema = RemoteRead.schema();
        assert_eq!(schema["properties"]["limit"]["maximum"], MAX_READ_LIMIT);
        let write = RemoteWrite.schema();
        assert_eq!(
            write["properties"]["content"]["maxLength"],
            MAX_WRITE_CONTENT_CHARS
        );
        let exec = RemoteExec.schema();
        assert_eq!(
            exec["properties"]["output_cap_bytes"]["maximum"],
            MAX_EXEC_CAP
        );
    }

    #[test]
    fn local_context_fails_explicitly() {
        let dir = tempfile::TempDir::new().unwrap();
        let context = ToolContext {
            root: dir.path().to_path_buf(),
            project_root: dir.path().to_path_buf(),
            sandbox_root: dir.path().join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: dir.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: None,
            process_supervisor: None,
        };
        let error = access(&context, &json!({"path": "a"})).unwrap_err();
        assert!(error.to_string().contains("no ssh execution host"));
    }
}

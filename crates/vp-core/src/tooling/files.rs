//! File discovery, inspection, patching, and structured file management.
//!
//! These tools absorb deterministic work that would otherwise become a chain
//! of `find`/`cat`/`sed`/`python` shell calls. Every write goes through the
//! canonical worktree jail and is atomic; every destructive operation supports
//! dry-run and an expected content hash.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use serde_json::{Value, json};

use crate::tooling::diff::render_diff;
use crate::tooling::fs::{
    atomic_write, display_relative, looks_binary, sha256_file, sha256_hex, utf8_line_count,
};
use crate::tools::{
    AgentTool, ToolContext, ToolOutput, path_is_git_metadata, safe_existing_path, safe_write_path,
    truncate,
};

const MAX_BATCH_READ_BYTES: usize = 512 * 1024;
const MAX_READ_FILES: usize = 20;
const MAX_FIND_RESULTS: usize = 2_000;
const MAX_PATCH_FILES: usize = 50;
const MAX_FILE_INFO_HASH_BYTES: u64 = 64 * 1024 * 1024;

pub struct FindFiles;
pub struct FileInfo;
pub struct ReadFiles;
pub struct ApplyPatch;
pub struct FileManage;

#[async_trait]
impl AgentTool for FindFiles {
    fn name(&self) -> &'static str {
        "find_files"
    }

    fn description(&self) -> &'static str {
        "Find files or directories recursively with a glob. Respects .gitignore by default and supports pagination."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob such as **/*.rs or src/**/*.swift; default **/*"},
                "path": {"type": "string", "description": "Directory relative to the worktree root; default ."},
                "kind": {"type": "string", "enum": ["file", "dir", "any"], "description": "Default file"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Default 200"},
                "cursor": {"type": "integer", "minimum": 0, "description": "Zero-based result offset"},
                "include_hidden": {"type": "boolean", "description": "Include dotfiles; default false"},
                "include_ignored": {"type": "boolean", "description": "Include .gitignore matches; default false"},
                "sort": {"type": "string", "enum": ["path", "size", "mtime"], "description": "Default path"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let pattern = input
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("**/*");
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = safe_existing_path(&context.root, relative)?;
        if !root.is_dir() {
            return Ok(ToolOutput::error_structured(
                "find_files path must be a directory",
                json!({"code": "INVALID_ARGUMENT", "path": root}),
            ));
        }
        let kind = input.get("kind").and_then(Value::as_str).unwrap_or("file");
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(1, MAX_FIND_RESULTS as u64) as usize;
        let cursor = input.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
        let include_hidden = input
            .get("include_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let include_ignored = input
            .get("include_ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let sort = input.get("sort").and_then(Value::as_str).unwrap_or("path");
        let globset = build_find_globset(pattern)?;

        let mut builder = WalkBuilder::new(&root);
        builder
            .hidden(!include_hidden)
            .git_ignore(!include_ignored)
            .git_global(!include_ignored)
            .git_exclude(!include_ignored)
            .require_git(false)
            .max_depth(Some(64));
        let mut matches = Vec::new();
        let mut scanned = 0usize;
        for entry in builder.build().flatten() {
            let path = entry.path();
            if path == root {
                continue;
            }
            if path_is_git_metadata(&context.root, path) {
                continue;
            }
            let Some(metadata) = entry.metadata().ok() else {
                continue;
            };
            let is_dir = metadata.is_dir();
            let is_file = metadata.is_file();
            if !(is_file || is_dir) {
                continue;
            }
            if kind == "file" && !is_file {
                continue;
            }
            if kind == "dir" && !is_dir {
                continue;
            }
            let relative_path = display_relative(&context.root, path);
            if !globset.is_match(&relative_path) && !globset.is_match(path) {
                continue;
            }
            scanned = scanned.saturating_add(1);
            matches.push(json!({
                "path": relative_path,
                "type": if is_dir { "dir" } else { "file" },
                "size_bytes": metadata.len(),
                "modified_ms": modified_ms(metadata.modified().ok()),
            }));
        }
        match sort {
            "size" => matches.sort_by(|left, right| {
                right["size_bytes"]
                    .as_u64()
                    .cmp(&left["size_bytes"].as_u64())
                    .then_with(|| left["path"].as_str().cmp(&right["path"].as_str()))
            }),
            "mtime" => matches.sort_by(|left, right| {
                right["modified_ms"]
                    .as_u64()
                    .cmp(&left["modified_ms"].as_u64())
                    .then_with(|| left["path"].as_str().cmp(&right["path"].as_str()))
            }),
            _ => matches.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str())),
        }
        let total = matches.len();
        let page = matches
            .into_iter()
            .skip(cursor)
            .take(limit)
            .collect::<Vec<_>>();
        let next_cursor = cursor.saturating_add(page.len());
        let truncated = next_cursor < total;
        let rendered = page
            .iter()
            .map(|entry| {
                if entry["type"] == "dir" {
                    format!("{}/", entry["path"].as_str().unwrap_or_default())
                } else {
                    format!(
                        "{} ({} bytes)",
                        entry["path"].as_str().unwrap_or_default(),
                        entry["size_bytes"].as_u64().unwrap_or(0)
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut rendered = rendered;
        if truncated || cursor > 0 {
            rendered.push_str(&format!(
                "\n… showing results {}-{} of {}; next_cursor={}",
                cursor.saturating_add(1),
                next_cursor,
                total,
                next_cursor
            ));
        }
        Ok(ToolOutput::ok_structured_preserving(
            rendered,
            json!({
                "matches": page,
                "total": total,
                "scanned": scanned,
                "cursor": cursor,
                "next_cursor": truncated.then_some(next_cursor),
                "truncated": truncated,
            }),
            false,
        ))
    }
}

#[async_trait]
impl AgentTool for FileInfo {
    fn name(&self) -> &'static str {
        "file_info"
    }

    fn description(&self) -> &'static str {
        "Inspect one path: size, mode, MIME, hash, UTF-8/binary state, line count, symlink target, and Git tracking."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the worktree root"}
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let path = safe_existing_path(&context.root, relative)?;
        let lexical = Path::new(relative);
        let link_path = if !lexical.is_absolute()
            && !lexical
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            context
                .root
                .canonicalize()
                .ok()
                .map(|root| root.join(lexical))
        } else {
            None
        };
        let metadata = link_path
            .as_deref()
            .and_then(|candidate| std::fs::symlink_metadata(candidate).ok())
            .unwrap_or_else(|| {
                std::fs::symlink_metadata(&path)
                    .with_context(|| format!("reading metadata for {}", path.display()))
                    .unwrap_or_else(|_| {
                        std::fs::metadata(&path).unwrap_or_else(|_| {
                            std::fs::symlink_metadata(&path).expect("metadata was checked above")
                        })
                    })
            });
        let is_symlink = metadata.file_type().is_symlink();
        let symlink_target = if is_symlink {
            link_path
                .as_deref()
                .or(Some(path.as_path()))
                .and_then(|candidate| std::fs::read_link(candidate).ok())
                .map(|target| target.to_string_lossy().to_string())
        } else {
            None
        };
        let followed = std::fs::metadata(&path).ok();
        let is_dir = followed.as_ref().is_some_and(std::fs::Metadata::is_dir);
        let size = followed.as_ref().map(std::fs::Metadata::len).unwrap_or(0);
        let mut sha256 = None;
        let mut is_binary = None;
        let mut line_count = None;
        let mut mime = mime_for_path(&path).to_string();
        if !is_dir && size <= MAX_FILE_INFO_HASH_BYTES {
            let bytes = std::fs::read(&path).ok();
            if let Some(bytes) = bytes {
                sha256 = Some(sha256_hex(&bytes));
                is_binary = Some(looks_binary(&bytes));
                if !looks_binary(&bytes)
                    && let Ok(text) = std::str::from_utf8(&bytes)
                {
                    line_count = Some(utf8_line_count(text));
                }
                if mime == "application/octet-stream"
                    && let Some(detected) = mime_for_bytes(&bytes)
                {
                    mime = detected.to_string();
                }
            }
        }
        let git = git_file_state(&context.root, &path);
        let summary = format!(
            "{} · {} · {} bytes · {}{}{}{}",
            display_relative(&context.root, &path),
            if is_dir {
                "directory"
            } else if is_symlink {
                "symlink"
            } else {
                "file"
            },
            size,
            mime,
            sha256
                .as_ref()
                .map(|hash| format!(" · sha256 {hash}"))
                .unwrap_or_default(),
            line_count
                .map(|lines| format!(" · {lines} lines"))
                .unwrap_or_default(),
            is_binary
                .map(|binary| if binary { " · binary" } else { "" })
                .unwrap_or_default()
        );
        Ok(ToolOutput::ok_structured(
            summary,
            json!({
                "path": display_relative(&context.root, &path),
                "absolute_path": path,
                "type": if is_dir { "dir" } else if is_symlink { "symlink" } else { "file" },
                "size_bytes": size,
                "mode": mode_string(&metadata),
                "read_only": metadata.permissions().readonly(),
                "modified_ms": modified_ms(metadata.modified().ok()),
                "mime_type": mime,
                "sha256": sha256,
                "binary": is_binary,
                "line_count": line_count,
                "symlink_target": symlink_target,
                "git": git,
            }),
        ))
    }
}

#[async_trait]
impl AgentTool for ReadFiles {
    fn name(&self) -> &'static str {
        "read_files"
    }

    fn description(&self) -> &'static str {
        "Read several UTF-8 files or line ranges in one call. Returns per-file hashes and truncation metadata."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "maxItems": MAX_READ_FILES,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "offset": {"type": "integer", "minimum": 0, "description": "Zero-based start line"},
                            "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Maximum lines; default 400"}
                        },
                        "required": ["path"]
                    }
                }
            },
            "required": ["files"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let files = input
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("files is required"))?;
        if files.is_empty() {
            return Ok(ToolOutput::error_structured(
                "files must not be empty",
                json!({"code": "INVALID_ARGUMENT"}),
            ));
        }
        if files.len() > MAX_READ_FILES {
            return Ok(ToolOutput::error_structured(
                format!("read_files accepts at most {MAX_READ_FILES} files per call"),
                json!({"code": "INVALID_ARGUMENT", "requested": files.len()}),
            ));
        }
        let mut results = Vec::new();
        let mut rendered = Vec::new();
        let mut total_bytes = 0usize;
        let mut truncated = false;
        for request in files {
            let relative = request
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("files[].path is required"))?;
            let offset = request.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let limit = request
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(400)
                .clamp(1, 2_000) as usize;
            match safe_existing_path(&context.root, relative) {
                Ok(path) => {
                    let bytes = tokio::fs::read(&path).await?;
                    if bytes.len() > 4 * 1024 * 1024 {
                        results.push(json!({
                            "path": relative,
                            "ok": false,
                            "error": "file exceeds 4 MiB read cap",
                            "size_bytes": bytes.len(),
                        }));
                        continue;
                    }
                    let Ok(content) = String::from_utf8(bytes) else {
                        results.push(json!({
                            "path": relative,
                            "ok": false,
                            "error": "file is not valid UTF-8",
                        }));
                        continue;
                    };
                    let total_lines = content.lines().count();
                    let lines = content
                        .lines()
                        .skip(offset)
                        .take(limit)
                        .enumerate()
                        .map(|(index, line)| format!("{:>5} | {}", offset + index + 1, line))
                        .collect::<Vec<_>>();
                    let file_truncated =
                        offset > 0 || offset.saturating_add(lines.len()) < total_lines;
                    total_bytes = total_bytes.saturating_add(content.len());
                    if total_bytes > MAX_BATCH_READ_BYTES {
                        truncated = true;
                    }
                    let hash = sha256_hex(content.as_bytes());
                    rendered.push(format!(
                        "== {} ({} lines, sha256 {}) ==\n{}",
                        relative,
                        total_lines,
                        &hash[..12.min(hash.len())],
                        lines.join("\n")
                    ));
                    results.push(json!({
                        "path": relative,
                        "ok": true,
                        "sha256": hash,
                        "total_lines": total_lines,
                        "offset": offset,
                        "lines": lines.len(),
                        "truncated": file_truncated,
                        "next_offset": file_truncated.then_some(offset.saturating_add(lines.len())),
                        "bytes_read": content.len(),
                    }));
                }
                Err(error) => {
                    results.push(json!({
                        "path": relative,
                        "ok": false,
                        "error": error.to_string(),
                    }));
                }
            }
        }
        Ok(ToolOutput::ok_structured_preserving(
            rendered.join("\n\n"),
            json!({
                "files": results,
                "total_bytes": total_bytes,
                "truncated": truncated || total_bytes > MAX_BATCH_READ_BYTES,
                "max_batch_bytes": MAX_BATCH_READ_BYTES,
            }),
            false,
        ))
    }
}

#[derive(Debug, Clone)]
struct PatchLine {
    kind: PatchLineKind,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchLineKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Clone)]
struct PatchHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<PatchLine>,
}

#[derive(Debug, Clone)]
struct FilePatch {
    old_path: Option<String>,
    new_path: Option<String>,
    hunks: Vec<PatchHunk>,
}

impl FilePatch {
    fn old_path(&self) -> Option<&str> {
        self.old_path.as_deref().filter(|path| *path != "/dev/null")
    }

    fn new_path(&self) -> Option<&str> {
        self.new_path.as_deref().filter(|path| *path != "/dev/null")
    }

    fn target_path(&self) -> Result<&str> {
        self.new_path()
            .or_else(|| self.old_path())
            .ok_or_else(|| anyhow!("patch file has no target path"))
    }
}

#[async_trait]
impl AgentTool for ApplyPatch {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Apply a unified diff to one or more worktree files atomically. Validates every file before writing and rolls back on failure."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {"type": "string", "description": "Unified diff text (git diff or plain diff)"},
                "dry_run": {"type": "boolean", "description": "Validate and report without writing"},
                "create_files": {"type": "boolean", "description": "Allow patches that create files; default true"},
                "delete_files": {"type": "boolean", "description": "Allow patches that delete files; default false"},
                "expected_hashes": {"type": "object", "description": "Map of path -> sha256 required before applying"},
                "max_files": {"type": "integer", "minimum": 1, "maximum": MAX_PATCH_FILES, "description": "Default 20"}
            },
            "required": ["patch"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let patch = input
            .get("patch")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("patch is required"))?;
        let dry_run = input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let create_files = input
            .get("create_files")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let delete_files = input
            .get("delete_files")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_files = input
            .get("max_files")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, MAX_PATCH_FILES as u64) as usize;
        let patches = match parse_unified_diff(patch) {
            Ok(patches) => patches,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    format!("patch parse error: {error}"),
                    json!({"code": "PATCH_CONFLICT", "message": error.to_string()}),
                ));
            }
        };
        if patches.is_empty() {
            return Ok(ToolOutput::error_structured(
                "patch contains no file hunks",
                json!({"code": "INVALID_ARGUMENT"}),
            ));
        }
        if patches.len() > max_files {
            return Ok(ToolOutput::error_structured(
                format!(
                    "patch touches {} files, above max_files={max_files}",
                    patches.len()
                ),
                json!({"code": "INVALID_ARGUMENT", "files": patches.len()}),
            ));
        }
        let expected_hashes = input
            .get("expected_hashes")
            .and_then(Value::as_object)
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(path, value)| {
                        value.as_str().map(|hash| (path.clone(), hash.to_string()))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();

        struct Planned {
            path: String,
            absolute: PathBuf,
            before: Option<String>,
            after: Option<String>,
            added: usize,
            deleted: usize,
            created: bool,
            deleted_file: bool,
        }

        let mut planned = Vec::new();
        let mut seen = HashSet::new();
        for file_patch in &patches {
            let target = file_patch.target_path()?.to_string();
            if !seen.insert(target.clone()) {
                return Ok(ToolOutput::error_structured(
                    format!("patch touches {target} more than once"),
                    json!({"code": "PATCH_CONFLICT", "path": target}),
                ));
            }
            let absolute = match safe_write_path(&context.root, &target) {
                Ok(path) => path,
                Err(error) => {
                    return Ok(ToolOutput::error_structured(
                        format!("invalid patch path {target}: {error}"),
                        json!({"code": "PATH_ESCAPE", "path": target}),
                    ));
                }
            };
            if path_is_git_metadata(&context.root, &absolute) {
                return Ok(ToolOutput::error_structured(
                    format!("refusing to patch Git metadata: {target}"),
                    json!({"code": "PATH_ESCAPE", "path": target}),
                ));
            }
            let before = match tokio::fs::read_to_string(&absolute).await {
                Ok(text) => Some(text),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            if let Some(expected) = expected_hashes.get(&target) {
                let Some(before) = before.as_deref() else {
                    return Ok(ToolOutput::error_structured(
                        format!("expected_hashes contains {target}, but the file does not exist"),
                        json!({"code": "HASH_MISMATCH", "path": target}),
                    ));
                };
                let actual = sha256_hex(before.as_bytes());
                if &actual != expected {
                    return Ok(ToolOutput::error_structured(
                        format!("hash mismatch for {target}"),
                        json!({
                            "code": "HASH_MISMATCH",
                            "path": target,
                            "expected_sha256": expected,
                            "actual_sha256": actual,
                        }),
                    ));
                }
            }
            let created = file_patch.old_path().is_none();
            let deleted_file = file_patch.new_path().is_none();
            if created && !create_files {
                return Ok(ToolOutput::error_structured(
                    format!("patch creates {target}, but create_files is false"),
                    json!({"code": "PATCH_CONFLICT", "path": target}),
                ));
            }
            if deleted_file && !delete_files {
                return Ok(ToolOutput::error_structured(
                    format!("patch deletes {target}, but delete_files is false"),
                    json!({"code": "PATCH_CONFLICT", "path": target}),
                ));
            }
            if created && before.is_some() {
                return Ok(ToolOutput::error_structured(
                    format!("patch creates {target}, but the file already exists"),
                    json!({"code": "PATCH_CONFLICT", "path": target}),
                ));
            }
            if !created && before.is_none() {
                return Ok(ToolOutput::error_structured(
                    format!("patch modifies {target}, but the file does not exist"),
                    json!({"code": "PATCH_CONFLICT", "path": target}),
                ));
            }
            let (after, added, deleted) = if deleted_file {
                if !file_patch.hunks.is_empty() {
                    return Ok(ToolOutput::error_structured(
                        format!("deletion patch for {target} must not contain hunks"),
                        json!({"code": "PATCH_CONFLICT", "path": target}),
                    ));
                }
                (None, 0, before.as_deref().map(utf8_line_count).unwrap_or(0))
            } else {
                let original = before.clone().unwrap_or_default();
                match apply_hunks(&original, &file_patch.hunks) {
                    Ok((text, added, deleted)) => (Some(text), added, deleted),
                    Err(error) => {
                        return Ok(ToolOutput::error_structured(
                            format!("failed to apply patch to {target}: {error}"),
                            json!({"code": "PATCH_CONFLICT", "path": target, "message": error.to_string()}),
                        ));
                    }
                }
            };
            planned.push(Planned {
                path: target,
                absolute,
                before,
                after,
                added,
                deleted,
                created,
                deleted_file,
            });
        }

        if !dry_run {
            let mut written: Vec<&Planned> = Vec::new();
            for item in &planned {
                let result = if item.deleted_file {
                    tokio::fs::remove_file(&item.absolute)
                        .await
                        .map_err(anyhow::Error::from)
                } else {
                    match item.after.as_deref() {
                        Some(after) => atomic_write(&item.absolute, after.as_bytes()),
                        None => Ok(()),
                    }
                };
                if let Err(error) = result {
                    for prior in written.iter().rev() {
                        let restore = match &prior.before {
                            Some(before) => atomic_write(&prior.absolute, before.as_bytes()),
                            None if prior.absolute.exists() => {
                                std::fs::remove_file(&prior.absolute).map_err(Into::into)
                            }
                            None => Ok(()),
                        };
                        let _ = restore;
                    }
                    return Err(error.context(format!("writing {}", item.path)));
                }
                written.push(item);
            }
        }

        if !dry_run {
            crate::tooling::code::invalidate_index(&context.root);
        }
        let mut files = Vec::new();
        let mut diffs = Vec::new();
        let mut summary_added = 0usize;
        let mut summary_deleted = 0usize;
        for item in &planned {
            summary_added = summary_added.saturating_add(item.added);
            summary_deleted = summary_deleted.saturating_add(item.deleted);
            let status = if item.created {
                "created"
            } else if item.deleted_file {
                "deleted"
            } else {
                "modified"
            };
            files.push(json!({
                "path": item.path,
                "status": status,
                "additions": item.added,
                "deletions": item.deleted,
                "before_sha256": item.before.as_deref().map(|text| sha256_hex(text.as_bytes())),
                "after_sha256": item.after.as_deref().map(|text| sha256_hex(text.as_bytes())),
            }));
            if !item.deleted_file
                && let (Some(before), Some(after)) = (item.before.as_deref(), item.after.as_deref())
                && before != after
            {
                let diff = render_diff(before, after, 3);
                if !diff.is_empty() {
                    diffs.push(format!(
                        "--- a/{}\n+++ b/{}\n{}",
                        item.path, item.path, diff
                    ));
                }
            }
        }
        let summary = format!(
            "{} files changed, +{summary_added} -{summary_deleted}",
            files.len()
        );
        Ok(ToolOutput::ok_structured(
            truncate(format!(
                "{}: {}\n{}",
                if dry_run {
                    "dry-run patch validated"
                } else {
                    "patch applied"
                },
                summary,
                diffs.join("\n")
            )),
            json!({
                "ok": true,
                "dry_run": dry_run,
                "files": files,
                "summary": summary,
                "diff": truncate(diffs.join("\n")),
                "warnings": [],
                "next_actions": if dry_run {
                    vec!["rerun with dry_run=false to write the patch"]
                } else {
                    vec!["run_project_task test"]
                },
            }),
        ))
    }
}

fn build_find_globset(pattern: &str) -> Result<globset::GlobSet> {
    let pattern = pattern.trim().strip_prefix("./").unwrap_or(pattern.trim());
    if pattern.is_empty() {
        bail!("pattern must not be empty");
    }
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(pattern)?);
    if !pattern.contains('/') {
        builder.add(Glob::new(&format!("**/{pattern}"))?);
    }
    Ok(builder.build()?)
}

fn parse_unified_diff(text: &str) -> Result<Vec<FilePatch>> {
    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut hunk: Option<PatchHunk> = None;
    for raw in text.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(active) = hunk.as_ref()
            && !hunk_complete(active)
        {
            push_patch_line(hunk.as_mut().expect("checked hunk"), line);
            continue;
        }
        if line.starts_with("diff --git ") {
            finish_hunk(&mut current, &mut hunk)?;
            if current.as_ref().is_some_and(|file| file.new_path.is_some()) {
                finish_file(&mut files, &mut current)?;
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("--- ") {
            finish_hunk(&mut current, &mut hunk)?;
            if current.as_ref().is_some_and(|file| file.new_path.is_some()) {
                finish_file(&mut files, &mut current)?;
            }
            let old = parse_diff_path(value)?;
            current
                .get_or_insert_with(|| FilePatch {
                    old_path: None,
                    new_path: None,
                    hunks: Vec::new(),
                })
                .old_path = Some(old);
            continue;
        }
        if let Some(value) = line.strip_prefix("+++ ") {
            let new = parse_diff_path(value)?;
            let file = current
                .as_mut()
                .ok_or_else(|| anyhow!("+++ header without a preceding --- header"))?;
            file.new_path = Some(new);
            continue;
        }
        if line.starts_with("@@") {
            finish_hunk(&mut current, &mut hunk)?;
            let file = current
                .as_mut()
                .ok_or_else(|| anyhow!("hunk appears before a file header"))?;
            let (old_start, old_count, new_start, new_count) = parse_hunk_header(line)?;
            hunk = Some(PatchHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
            let _ = file;
            continue;
        }
    }
    finish_hunk(&mut current, &mut hunk)?;
    if current.as_ref().is_some_and(|file| file.new_path.is_some()) {
        finish_file(&mut files, &mut current)?;
    }
    if let Some(file) = current {
        return Err(anyhow!(
            "incomplete file header for {}",
            file.old_path.as_deref().unwrap_or("?")
        ));
    }
    Ok(files)
}

fn hunk_complete(hunk: &PatchHunk) -> bool {
    let old_lines = hunk
        .lines
        .iter()
        .filter(|line| line.kind != PatchLineKind::Add)
        .count();
    let new_lines = hunk
        .lines
        .iter()
        .filter(|line| line.kind != PatchLineKind::Delete)
        .count();
    old_lines == hunk.old_count && new_lines == hunk.new_count
}

fn push_patch_line(hunk: &mut PatchHunk, line: &str) {
    if line.starts_with('\\') {
        return;
    }
    if line.is_empty() {
        hunk.lines.push(PatchLine {
            kind: PatchLineKind::Context,
            text: String::new(),
        });
        return;
    }
    let mut chars = line.chars();
    let prefix = chars.next().unwrap_or(' ');
    let text = chars.as_str().to_string();
    let kind = match prefix {
        ' ' => PatchLineKind::Context,
        '+' => PatchLineKind::Add,
        '-' => PatchLineKind::Delete,
        _ => return,
    };
    hunk.lines.push(PatchLine { kind, text });
}

fn finish_hunk(current: &mut Option<FilePatch>, hunk: &mut Option<PatchHunk>) -> Result<()> {
    let Some(hunk) = hunk.take() else {
        return Ok(());
    };
    let file = current
        .as_mut()
        .ok_or_else(|| anyhow!("hunk without a file header"))?;
    let old_lines = hunk
        .lines
        .iter()
        .filter(|line| line.kind != PatchLineKind::Add)
        .count();
    let new_lines = hunk
        .lines
        .iter()
        .filter(|line| line.kind != PatchLineKind::Delete)
        .count();
    if old_lines != hunk.old_count || new_lines != hunk.new_count {
        return Err(anyhow!(
            "hunk @@ -{},{} +{},{} @@ declares {} old / {} new lines but contains {} / {}",
            hunk.old_start,
            hunk.old_count,
            hunk.new_start,
            hunk.new_count,
            hunk.old_count,
            hunk.new_count,
            old_lines,
            new_lines
        ));
    }
    file.hunks.push(hunk);
    Ok(())
}

fn finish_file(files: &mut Vec<FilePatch>, current: &mut Option<FilePatch>) -> Result<()> {
    let Some(file) = current.take() else {
        return Ok(());
    };
    if file.new_path.is_none() {
        return Err(anyhow!(
            "file header for {} has no +++ path",
            file.old_path.as_deref().unwrap_or("?")
        ));
    }
    if file.old_path.as_deref() == Some("/dev/null")
        && file.new_path.as_deref() == Some("/dev/null")
    {
        return Err(anyhow!("patch file has /dev/null on both sides"));
    }
    if file.hunks.is_empty()
        && file.old_path.as_deref() != Some("/dev/null")
        && file.new_path.as_deref() != Some("/dev/null")
    {
        return Err(anyhow!(
            "patch for {} contains no hunks",
            file.target_path()?
        ));
    }
    files.push(file);
    Ok(())
}

fn parse_hunk_header(line: &str) -> Result<(usize, usize, usize, usize)> {
    let rest = line
        .strip_prefix("@@")
        .ok_or_else(|| anyhow!("invalid hunk header"))?;
    let end = rest
        .find("@@")
        .ok_or_else(|| anyhow!("hunk header is missing the closing @@"))?;
    let mut parts = rest[..end].split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| anyhow!("hunk header is missing the old range"))?;
    let new = parts
        .next()
        .ok_or_else(|| anyhow!("hunk header is missing the new range"))?;
    let (old_start, old_count) = parse_hunk_range(old, '-')?;
    let (new_start, new_count) = parse_hunk_range(new, '+')?;
    Ok((old_start, old_count, new_start, new_count))
}

fn parse_hunk_range(value: &str, prefix: char) -> Result<(usize, usize)> {
    let value = value
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow!("hunk range must start with {prefix}"))?;
    let (start, count) = match value.split_once(',') {
        Some((start, count)) => (start, count.parse::<usize>()?),
        None => (value, 1),
    };
    let start = start.parse::<usize>()?;
    Ok((start, count))
}

fn parse_diff_path(value: &str) -> Result<String> {
    let value = value.split('\t').next().unwrap_or(value).trim().to_string();
    if value.is_empty() {
        bail!("empty diff path");
    }
    let unquoted = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        unescape_diff_path(&value[1..value.len() - 1])?
    } else {
        value
    };
    let path = unquoted
        .strip_prefix("a/")
        .or_else(|| unquoted.strip_prefix("b/"))
        .unwrap_or(&unquoted)
        .to_string();
    if path.is_empty() || path == "/dev/null" {
        return Ok(path);
    }
    if Path::new(&path).is_absolute() {
        bail!("absolute paths are not allowed in patches: {path}");
    }
    Ok(path)
}

fn unescape_diff_path(value: &str) -> Result<String> {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    Ok(output)
}

struct TextDoc {
    lines: Vec<String>,
    newline: &'static str,
    trailing_newline: bool,
}

fn split_text_doc(text: &str) -> TextDoc {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let normalized = text.replace("\r\n", "\n");
    let trailing_newline = normalized.ends_with('\n');
    let lines = normalized
        .split_terminator('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    TextDoc {
        lines,
        newline,
        trailing_newline,
    }
}

fn apply_hunks(original: &str, hunks: &[PatchHunk]) -> Result<(String, usize, usize)> {
    let mut doc = split_text_doc(original);
    let mut delta: isize = 0;
    let mut added = 0usize;
    let mut deleted = 0usize;
    for hunk in hunks {
        let old_sequence = hunk
            .lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Add)
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        let new_sequence = hunk
            .lines
            .iter()
            .filter(|line| line.kind != PatchLineKind::Delete)
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        added = added.saturating_add(
            hunk.lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Add)
                .count(),
        );
        deleted = deleted.saturating_add(
            hunk.lines
                .iter()
                .filter(|line| line.kind == PatchLineKind::Delete)
                .count(),
        );
        let position = if old_sequence.is_empty() {
            let expected = hunk.old_start as isize + delta;
            expected.max(0) as usize
        } else {
            if old_sequence.len() > doc.lines.len() {
                bail!(
                    "hunk expects {} lines at {}, but the file has {}",
                    old_sequence.len(),
                    hunk.old_start,
                    doc.lines.len()
                );
            }
            let expected = (hunk.old_start.saturating_sub(1) as isize + delta).max(0) as usize;
            let last = doc.lines.len() - old_sequence.len();
            let mut best: Option<usize> = None;
            for candidate in 0..=last {
                if doc.lines[candidate..candidate + old_sequence.len()]
                    .iter()
                    .map(String::as_str)
                    .eq(old_sequence.iter().copied())
                {
                    best = match best {
                        Some(current)
                            if current.abs_diff(expected) <= candidate.abs_diff(expected) =>
                        {
                            Some(current)
                        }
                        _ => Some(candidate),
                    };
                }
            }
            best.ok_or_else(|| {
                anyhow!(
                    "context does not match near line {}; the file may have changed",
                    hunk.old_start
                )
            })?
        };
        if position > doc.lines.len() {
            bail!("hunk insertion point is beyond the end of the file");
        }
        let end = position.saturating_add(old_sequence.len());
        doc.lines.splice(position..end, new_sequence);
        delta += hunk.new_count as isize - hunk.old_count as isize;
    }
    if added > 0 {
        doc.trailing_newline = true;
    }
    let mut output = doc.lines.join(doc.newline);
    if doc.trailing_newline && !output.is_empty() {
        output.push_str(doc.newline);
    }
    Ok((output, added, deleted))
}

#[async_trait]
impl AgentTool for FileManage {
    fn name(&self) -> &'static str {
        "file_manage"
    }

    fn description(&self) -> &'static str {
        "Create directories, move, copy, or delete files inside the worktree with dry-run and expected-hash support."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["mkdir", "move", "copy", "delete"]},
                "source": {"type": "string", "description": "Source path for move/copy/delete"},
                "destination": {"type": "string", "description": "Destination path for move/copy/mkdir"},
                "recursive": {"type": "boolean", "description": "Required for non-empty directory delete"},
                "overwrite": {"type": "boolean", "description": "Allow replacing an existing destination; default false"},
                "dry_run": {"type": "boolean", "description": "Validate and report without changing files"},
                "expected_hashes": {"type": "object", "description": "Map of source path -> sha256 before move/delete"}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        let dry_run = input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let overwrite = input
            .get("overwrite")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let recursive = input
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match action {
            "mkdir" => {
                let destination = input
                    .get("destination")
                    .or_else(|| input.get("source"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("destination is required"))?;
                let target = safe_write_path(&context.root, destination)?;
                if target.exists() && !target.is_dir() {
                    return Ok(ToolOutput::error_structured(
                        format!("{destination} exists and is not a directory"),
                        json!({"code": "PATCH_CONFLICT", "path": destination}),
                    ));
                }
                if !dry_run {
                    tokio::fs::create_dir_all(&target).await?;
                    crate::tooling::code::invalidate_index(&context.root);
                }
                Ok(ToolOutput::ok_structured(
                    format!(
                        "{} {}/",
                        if dry_run { "would create" } else { "created" },
                        destination
                    ),
                    json!({
                        "action": "mkdir",
                        "path": destination,
                        "dry_run": dry_run,
                        "created": !target.exists() || dry_run,
                    }),
                ))
            }
            "delete" => {
                let source = input
                    .get("source")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("source is required"))?;
                let target = safe_existing_path(&context.root, source)?;
                if path_is_git_metadata(&context.root, &target) {
                    return Ok(ToolOutput::error_structured(
                        "refusing to delete Git metadata",
                        json!({"code": "PATH_ESCAPE", "path": source}),
                    ));
                }
                verify_expected_hash(&input, source, &target)?;
                let metadata = std::fs::symlink_metadata(&target)?;
                if metadata.is_dir() {
                    let mut entries = std::fs::read_dir(&target)?;
                    if entries.next().is_some() && !recursive {
                        return Ok(ToolOutput::error_structured(
                            format!("{source} is a non-empty directory; set recursive=true"),
                            json!({"code": "RECURSIVE_REQUIRED", "path": source}),
                        ));
                    }
                }
                let affected = collect_paths(&target, 200).unwrap_or_default();
                if !dry_run {
                    if metadata.is_dir() && !metadata.file_type().is_symlink() {
                        tokio::fs::remove_dir_all(&target).await?;
                    } else {
                        tokio::fs::remove_file(&target).await?;
                    }
                    crate::tooling::code::invalidate_index(&context.root);
                }
                Ok(ToolOutput::ok_structured(
                    format!(
                        "{} {} ({} path(s))",
                        if dry_run { "would delete" } else { "deleted" },
                        source,
                        affected.len()
                    ),
                    json!({
                        "action": "delete",
                        "path": source,
                        "dry_run": dry_run,
                        "affected": affected,
                        "warnings": ["deleted files are not recoverable by this tool; tracked files can be restored from Git"],
                    }),
                ))
            }
            "move" | "copy" => {
                let source = input
                    .get("source")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("source is required"))?;
                let destination = input
                    .get("destination")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("destination is required"))?;
                let source_path = safe_existing_path(&context.root, source)?;
                if path_is_git_metadata(&context.root, &source_path) {
                    return Ok(ToolOutput::error_structured(
                        "refusing to manage Git metadata",
                        json!({"code": "PATH_ESCAPE", "path": source}),
                    ));
                }
                verify_expected_hash(&input, source, &source_path)?;
                let mut target = safe_write_path(&context.root, destination)?;
                if target.is_dir() {
                    let file_name = source_path
                        .file_name()
                        .ok_or_else(|| anyhow!("source has no file name"))?;
                    target = target.join(file_name);
                }
                if target.exists() && !overwrite {
                    return Ok(ToolOutput::error_structured(
                        format!("{destination} already exists; set overwrite=true"),
                        json!({"code": "PATCH_CONFLICT", "path": destination}),
                    ));
                }
                if target.starts_with(&source_path) {
                    return Ok(ToolOutput::error_structured(
                        "destination must not be inside the source",
                        json!({"code": "INVALID_ARGUMENT"}),
                    ));
                }
                let metadata = std::fs::metadata(&source_path)?;
                if metadata.is_dir() && !recursive {
                    let mut entries = std::fs::read_dir(&source_path)?;
                    if entries.next().is_some() {
                        return Ok(ToolOutput::error_structured(
                            format!("{source} is a non-empty directory; set recursive=true"),
                            json!({"code": "RECURSIVE_REQUIRED", "path": source}),
                        ));
                    }
                }
                if !dry_run {
                    if let Some(parent) = target.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    if action == "move" {
                        match tokio::fs::rename(&source_path, &target).await {
                            Ok(()) => {}
                            Err(_) => {
                                copy_path(&source_path, &target)?;
                                if metadata.is_dir() {
                                    tokio::fs::remove_dir_all(&source_path).await?;
                                } else {
                                    tokio::fs::remove_file(&source_path).await?;
                                }
                            }
                        }
                    } else {
                        copy_path(&source_path, &target)?;
                    }
                    crate::tooling::code::invalidate_index(&context.root);
                }
                Ok(ToolOutput::ok_structured(
                    format!(
                        "{} {} -> {}",
                        if dry_run {
                            "would move/copy"
                        } else if action == "move" {
                            "moved"
                        } else {
                            "copied"
                        },
                        source,
                        display_relative(&context.root, &target)
                    ),
                    json!({
                        "action": action,
                        "source": source,
                        "destination": display_relative(&context.root, &target),
                        "dry_run": dry_run,
                        "type": if metadata.is_dir() { "dir" } else { "file" },
                    }),
                ))
            }
            _ => Ok(ToolOutput::error_structured(
                format!("unsupported action: {action}"),
                json!({"code": "INVALID_ARGUMENT", "action": action}),
            )),
        }
    }
}

fn verify_expected_hash(input: &Value, relative: &str, path: &Path) -> Result<()> {
    let Some(expected) = input
        .get("expected_hashes")
        .and_then(Value::as_object)
        .and_then(|object| object.get(relative))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if path.is_dir() {
        bail!("expected_hashes is only supported for files: {relative}");
    }
    let actual = sha256_file(path)?;
    if actual != expected {
        bail!("hash mismatch for {relative}: expected {expected}, actual {actual}");
    }
    Ok(())
}

fn collect_paths(path: &Path, limit: usize) -> Result<Vec<String>> {
    let mut result = Vec::new();
    collect_paths_inner(path, &mut result, limit)?;
    Ok(result)
}

fn collect_paths_inner(path: &Path, result: &mut Vec<String>, limit: usize) -> Result<()> {
    if result.len() >= limit {
        return Ok(());
    }
    result.push(path.to_string_lossy().to_string());
    if path.is_dir() && !std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            collect_paths_inner(&entry.path(), result, limit)?;
            if result.len() >= limit {
                break;
            }
        }
    }
    Ok(())
}

fn copy_path(source: &Path, target: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let link = std::fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, target)?;
        #[cfg(not(unix))]
        std::fs::copy(source, target)?;
        return Ok(());
    }
    if metadata.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_path(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(source, target)?;
    }
    Ok(())
}

fn modified_ms(modified: Option<SystemTime>) -> Option<u64> {
    modified
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

fn mode_string(metadata: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        format!("{:o}", metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        if metadata.permissions().readonly() {
            "read-only".to_string()
        } else {
            "read-write".to_string()
        }
    }
}

pub(crate) fn mime_for_path(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "text/rust",
        "swift" => "text/swift",
        "ts" | "tsx" => "text/typescript",
        "js" | "jsx" | "mjs" | "cjs" => "text/javascript",
        "py" => "text/x-python",
        "go" => "text/x-go",
        "java" => "text/x-java",
        "kt" | "kts" => "text/x-kotlin",
        "c" | "h" => "text/x-c",
        "cc" | "cpp" | "hpp" => "text/x-c++",
        "json" => "application/json",
        "jsonl" => "application/x-ndjson",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "xml" => "application/xml",
        "sh" | "bash" | "zsh" => "text/x-shellscript",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "log" => "text/plain",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn mime_for_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF89a") || bytes.starts_with(b"GIF87a") {
        Some("image/gif")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if bytes.starts_with(b"PK\x03\x04") {
        Some("application/zip")
    } else if std::str::from_utf8(bytes).is_ok() {
        Some("text/plain")
    } else {
        None
    }
}

fn git_file_state(root: &Path, path: &Path) -> Value {
    let relative = match path.strip_prefix(root) {
        Ok(relative) => relative.to_string_lossy().to_string(),
        Err(_) => return json!({"available": false}),
    };
    let run = |args: &[&str]| -> Option<String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .arg("--")
            .arg(&relative)
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).to_string())
    };
    let tracked = run(&["ls-files", "--error-unmatch"])
        .map(|output| !output.trim().is_empty())
        .unwrap_or(false);
    let ignored = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--"])
        .arg(&relative)
        .status()
        .ok()
        .is_some_and(|status| status.success());
    let status = run(&["status", "--porcelain"])
        .map(|output| output.trim().to_string())
        .filter(|output| !output.is_empty());
    json!({
        "available": true,
        "tracked": tracked,
        "ignored": ignored,
        "status": status,
    })
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
        }
    }

    #[tokio::test]
    async fn finds_files_and_reports_metadata() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.path().join("src/b.txt"), "b\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "ignored\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        let output = FindFiles
            .execute(&context(dir.path()), json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("src/a.rs"));
        assert!(!output.content.contains("ignored.rs"));

        let info = FileInfo
            .execute(&context(dir.path()), json!({"path": "src/a.rs"}))
            .await
            .unwrap();
        assert!(!info.is_error);
        let structured = info.structured.unwrap();
        assert_eq!(structured["line_count"], 1);
        assert_eq!(
            structured["sha256"].as_str().unwrap(),
            sha256_hex(b"fn a() {}\n")
        );
    }

    #[tokio::test]
    async fn reads_multiple_files_in_one_call() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "three\n").unwrap();
        let output = ReadFiles
            .execute(
                &context(dir.path()),
                json!({"files": [
                    {"path": "a.txt", "offset": 1, "limit": 1},
                    {"path": "b.txt"}
                ]}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("two"));
        assert!(output.content.contains("three"));
        assert_eq!(
            output.structured.unwrap()["files"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn applies_multi_file_patch_atomically() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "alpha\nbeta\n").unwrap();
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
--- a/b.txt
+++ b/b.txt
@@ -1,2 +1,3 @@
 alpha
 beta
+gamma
";
        let output = ApplyPatch
            .execute(
                &context(dir.path()),
                json!({"patch": patch, "create_files": true}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "alpha\nbeta\ngamma\n"
        );
        assert!(output.content.contains("+2 -1"));
    }

    #[tokio::test]
    async fn rejects_stale_hashes_and_path_escapes() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-one
+two
";
        let mismatch = ApplyPatch
            .execute(
                &context(dir.path()),
                json!({
                    "patch": patch,
                    "expected_hashes": {"a.txt": "deadbeef"}
                }),
            )
            .await
            .unwrap();
        assert!(mismatch.is_error);
        assert!(mismatch.content.contains("hash mismatch"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );

        let escape = ApplyPatch
            .execute(
                &context(dir.path()),
                json!({"patch": "--- a/../outside.txt\n+++ b/../outside.txt\n@@ -1 +1 @@\n-x\n+y\n"}),
            )
            .await
            .unwrap();
        assert!(escape.is_error);
    }

    #[tokio::test]
    async fn does_not_confuse_deleted_dashes_with_file_headers() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "keep\n-- comment\nend\n").unwrap();
        let patch = "\
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,2 @@
 keep
--- comment
 end
";
        let output = ApplyPatch
            .execute(
                &context(dir.path()),
                json!({"patch": patch, "delete_files": false}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "keep\nend\n"
        );
    }

    #[tokio::test]
    async fn creates_and_deletes_files_from_patch() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("old.txt"), "old\n").unwrap();
        let create = "\
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let output = ApplyPatch
            .execute(
                &context(dir.path()),
                json!({"patch": create, "create_files": true}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello\nworld\n"
        );

        let delete = "\
--- a/old.txt
+++ /dev/null
";
        let output = ApplyPatch
            .execute(
                &context(dir.path()),
                json!({"patch": delete, "delete_files": true}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(!dir.path().join("old.txt").exists());
    }

    #[tokio::test]
    async fn manages_files_with_dry_run() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.txt"), "a\n").unwrap();
        let copied = FileManage
            .execute(
                &context(dir.path()),
                json!({"action": "copy", "source": "src/a.txt", "destination": "src/b.txt"}),
            )
            .await
            .unwrap();
        assert!(!copied.is_error, "{}", copied.content);
        assert!(dir.path().join("src/b.txt").exists());

        let moved = FileManage
            .execute(
                &context(dir.path()),
                json!({"action": "move", "source": "src/b.txt", "destination": "c.txt"}),
            )
            .await
            .unwrap();
        assert!(!moved.is_error, "{}", moved.content);
        assert!(!dir.path().join("src/b.txt").exists());

        let dry_run = FileManage
            .execute(
                &context(dir.path()),
                json!({"action": "delete", "source": "c.txt", "dry_run": true}),
            )
            .await
            .unwrap();
        assert!(!dry_run.is_error);
        assert!(dir.path().join("c.txt").exists());

        let deleted = FileManage
            .execute(
                &context(dir.path()),
                json!({"action": "delete", "source": "c.txt"}),
            )
            .await
            .unwrap();
        assert!(!deleted.is_error);
        assert!(!dir.path().join("c.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_symlink_metadata() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("target.txt"), "target\n").unwrap();
        std::os::unix::fs::symlink("target.txt", dir.path().join("link.txt")).unwrap();
        let output = FileInfo
            .execute(&context(dir.path()), json!({"path": "link.txt"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["type"], "symlink");
        assert_eq!(structured["symlink_target"], "target.txt");
    }
}

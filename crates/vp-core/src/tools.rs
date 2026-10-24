use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::timeout;
use vp_providers::ToolSchema;

const MAX_TOOL_OUTPUT: usize = 40_000;

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    fn requires_approval(&self) -> bool;
    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn AgentTool>>,
}

impl ToolRegistry {
    pub fn default_tools() -> Self {
        Self {
            tools: vec![
                Arc::new(ReadFile),
                Arc::new(ListDir),
                Arc::new(Grep),
                Arc::new(WriteFile),
                Arc::new(Shell),
            ],
        }
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .iter()
            .map(|tool| ToolSchema {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.schema(),
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.iter().find(|tool| tool.name() == name).cloned()
    }
}

struct ReadFile;
struct ListDir;
struct Grep;
struct WriteFile;
struct Shell;

#[async_trait]
impl AgentTool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file from the project. Returns line-numbered content."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the project root"},
                "offset": {"type": "integer", "minimum": 0, "description": "Zero-based starting line"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Maximum lines"}
            },
            "required": ["path"]
        })
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(400) as usize;
        let path = safe_existing_path(&context.root, path)?;
        let content = tokio::fs::read_to_string(&path).await?;
        let lines: Vec<&str> = content.lines().skip(offset).take(limit).collect();
        let mut out = format!("{} ({} lines)\n", path.display(), content.lines().count());
        for (index, line) in lines.iter().enumerate() {
            out.push_str(&format!("{:>5} | {}\n", offset + index + 1, line));
        }
        Ok(ToolOutput::ok(truncate(out)))
    }
}

#[async_trait]
impl AgentTool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List files and directories under a project-relative path."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the project root; default ."}
            }
        })
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let path = safe_existing_path(&context.root, relative)?;
        let mut entries = Vec::new();
        let mut reader = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = reader.next_entry().await? {
            let metadata = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            entries.push(if metadata.is_dir() {
                format!("{name}/")
            } else {
                format!("{name} ({} bytes)", metadata.len())
            });
        }
        entries.sort();
        Ok(ToolOutput::ok(truncate(entries.join("\n"))))
    }
}

#[async_trait]
impl AgentTool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search project files with a regular expression. Returns path:line matches."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Rust regex pattern"},
                "path": {"type": "string", "description": "Directory or file relative to project root; default ."},
                "glob": {"type": "string", "description": "Optional glob such as **/*.rs"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500}
            },
            "required": ["pattern"]
        })
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let pattern = input
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("pattern is required"))?;
        let regex = Regex::new(pattern).context("invalid regex")?;
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let search_path = safe_existing_path(&context.root, relative)?;
        let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
        let globset = input
            .get("glob")
            .and_then(Value::as_str)
            .map(build_globset)
            .transpose()?;

        let mut matches = Vec::new();
        let mut builder = WalkBuilder::new(&search_path);
        builder.hidden(false).git_ignore(true).require_git(false);
        for entry in builder.build().flatten() {
            if matches.len() >= limit {
                break;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.path();
            if let Some(globset) = &globset
                && !globset.is_match(path)
            {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            if content.contains('\0') {
                continue;
            }
            for (line_index, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    let relative = path.strip_prefix(&context.root).unwrap_or(path);
                    matches.push(format!(
                        "{}:{}: {}",
                        relative.display(),
                        line_index + 1,
                        line
                    ));
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(ToolOutput::ok(truncate(matches.join("\n"))))
    }
}

#[async_trait]
impl AgentTool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create or overwrite a UTF-8 file in the project. Requires approval."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the project root"},
                "content": {"type": "string", "description": "Full file content"}
            },
            "required": ["path", "content"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let content = input
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("content is required"))?;
        let path = safe_write_path(&context.root, path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        Ok(ToolOutput::ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            path.display()
        )))
    }
}

#[async_trait]
impl AgentTool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Run a non-interactive shell command in the project root. Requires approval."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Command to run with the user's shell"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 900}
            },
            "required": ["command"]
        })
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("command is required"))?;
        let lower = command.to_lowercase();
        for blocked in ["sudo ", "rm -rf /", "curl | sh", "wget | sh", "> /dev/"] {
            if lower.contains(blocked) {
                return Ok(ToolOutput::error(format!(
                    "blocked command pattern: {blocked}"
                )));
            }
        }
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(120)
            .min(900);
        let output = timeout(
            Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-lc")
                .arg(command)
                .current_dir(&context.root)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| anyhow!("command timed out after {timeout_secs}s"))??;

        let mut text = String::new();
        if !output.stdout.is_empty() {
            text.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        let exit = output.status.code().unwrap_or(-1);
        let result = format!("exit {exit}\n{}", truncate(text));
        if output.status.success() {
            Ok(ToolOutput::ok(result))
        } else {
            Ok(ToolOutput::error(result))
        }
    }
}

fn build_globset(pattern: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(pattern)?);
    Ok(builder.build()?)
}

fn safe_existing_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(anyhow!("absolute paths are not allowed"));
    }
    let path = root
        .join(path)
        .canonicalize()
        .context("path does not exist")?;
    if !path.starts_with(&root) {
        return Err(anyhow!("path escapes the project root"));
    }
    Ok(path)
}

fn safe_write_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(anyhow!("absolute paths are not allowed"));
    }
    if path.components().any(|component| {
        !matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return Err(anyhow!("path must stay inside the project root"));
    }
    if path.file_name().is_none() {
        return Err(anyhow!("file name is required"));
    }
    let target = root.join(path);
    let mut ancestor = target
        .parent()
        .ok_or_else(|| anyhow!("parent directory is required"))?
        .to_path_buf();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow!("no existing ancestor for path"))?
            .to_path_buf();
    }
    let canonical_ancestor = ancestor.canonicalize()?;
    if !canonical_ancestor.starts_with(&root) {
        return Err(anyhow!("path escapes the project root"));
    }
    if target
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(anyhow!("refusing to write through a symlink"));
    }
    Ok(target)
}

fn truncate(mut content: String) -> String {
    if content.len() > MAX_TOOL_OUTPUT {
        content.truncate(MAX_TOOL_OUTPUT);
        content.push_str("\n… output truncated");
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx() -> (TempDir, ToolContext) {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
        let root = dir.path().to_path_buf();
        (dir, ToolContext { root })
    }

    #[tokio::test]
    async fn reads_files_and_rejects_escape() {
        let (_dir, context) = ctx();
        let output = ReadFile
            .execute(&context, json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert!(output.content.contains("hello"));
        let error = ReadFile
            .execute(&context, json!({"path": "../../etc/passwd"}))
            .await;
        assert!(error.is_err());
    }

    #[tokio::test]
    async fn greps_files() {
        let (_dir, context) = ctx();
        let output = Grep
            .execute(&context, json!({"pattern": "world", "path": "."}))
            .await
            .unwrap();
        assert!(output.content.contains("hello.txt:2"));
    }

    #[tokio::test]
    async fn writes_files_and_runs_shell() {
        let (_dir, context) = ctx();
        let written = WriteFile
            .execute(&context, json!({"path": "new.txt", "content": "new"}))
            .await
            .unwrap();
        assert!(!written.is_error);
        let nested = WriteFile
            .execute(
                &context,
                json!({"path": "src/deep/new.txt", "content": "nested"}),
            )
            .await
            .unwrap();
        assert!(!nested.is_error);
        assert!(context.root.join("src/deep/new.txt").exists());
        let shell = Shell
            .execute(&context, json!({"command": "cat new.txt"}))
            .await
            .unwrap();
        assert!(shell.content.contains("new"));
        let escape = WriteFile
            .execute(&context, json!({"path": "../outside.txt", "content": "x"}))
            .await;
        assert!(escape.is_err());
    }

    #[test]
    fn write_and_shell_require_approval() {
        assert!(WriteFile.requires_approval());
        assert!(Shell.requires_approval());
        assert!(!ReadFile.requires_approval());
    }
}

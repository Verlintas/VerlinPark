use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
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
                Arc::new(EditFile),
                Arc::new(WriteFile),
                Arc::new(Shell),
                Arc::new(FetchUrl),
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
struct EditFile;
struct WriteFile;
struct Shell;
struct FetchUrl;

fn is_blocked_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_ipv6(ip),
    }
}

fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    let [a, b, c, _] = octets;
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 203 && b == 0 && c == 113)
        || a >= 240
}

fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(mapped);
    }
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        || (segments[0] & 0xffc0) == 0xfec0
}

async fn resolve_public_address(host: &str, port: u16) -> Result<SocketAddr> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("resolving {host}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(anyhow!("host did not resolve: {host}"));
    }
    if addresses.iter().any(|address| is_blocked_ip(address.ip())) {
        return Err(anyhow!("host resolves to a private or local address"));
    }
    // Prefer IPv4 when both families are available; a v6-first address can
    // fail on machines without a working IPv6 route even though v4 works.
    Ok(addresses
        .iter()
        .find(|address| address.is_ipv4())
        .copied()
        .unwrap_or(addresses[0]))
}

#[async_trait]
impl AgentTool for FetchUrl {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn description(&self) -> &'static str {
        "Fetch a public HTTP(S) URL for research. Returns text content, truncated."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Public http(s) URL"},
                "max_chars": {"type": "integer", "minimum": 100, "maximum": 20000}
            },
            "required": ["url"]
        })
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(&self, _context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let raw_url = input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("url is required"))?;
        let max_chars = input
            .get("max_chars")
            .and_then(Value::as_u64)
            .unwrap_or(8000)
            .clamp(100, 20000) as usize;
        let url = reqwest::Url::parse(raw_url).context("invalid URL")?;
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Ok(ToolOutput::error(format!(
                    "unsupported URL scheme: {other}"
                )));
            }
        }
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("URL has no host"))?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if is_blocked_host(&host) {
            return Ok(ToolOutput::error("local hosts are blocked"));
        }
        let port = url.port_or_known_default().unwrap_or(80);
        let mut client_builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .user_agent("VerlinPark/0.1 research fetcher");
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                return Ok(ToolOutput::error(
                    "private or local IP addresses are blocked",
                ));
            }
        } else {
            // Resolve and pin the address that was actually checked. Checking
            // only the hostname would let a public name resolve to loopback or
            // a private network between validation and connection.
            let address = resolve_public_address(&host, port).await?;
            client_builder = client_builder.resolve(&host, address);
        }
        let client = client_builder.build().context("building HTTP client")?;
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| anyhow!("fetch failed: {error}"))?;
        let status = response.status();
        if status.is_redirection() {
            return Ok(ToolOutput::error(
                "redirects are not followed; request the final URL explicitly",
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let mut body = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() >= max_chars * 4 {
                break;
            }
        }
        let text = String::from_utf8_lossy(&body);
        let text = truncate(text.to_string());
        let mut truncated = text.chars().take(max_chars).collect::<String>();
        if truncated.len() < text.len() && !truncated.is_empty() {
            truncated.push_str("\n… content truncated");
        }
        Ok(ToolOutput::ok(format!(
            "GET {raw_url} -> {status} ({content_type})\n{truncated}"
        )))
    }
}

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
impl AgentTool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in an existing UTF-8 file. Requires approval."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the worktree root"},
                "old_string": {"type": "string", "description": "Exact text to replace"},
                "new_string": {"type": "string", "description": "Replacement text"},
                "replace_all": {"type": "boolean", "description": "Replace every match; default false"}
            },
            "required": ["path", "old_string", "new_string"]
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
        let old_string = input
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("old_string is required"))?;
        let new_string = input
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("new_string is required"))?;
        let replace_all = input
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if old_string.is_empty() {
            return Ok(ToolOutput::error("old_string must not be empty"));
        }
        let path = safe_existing_path(&context.root, path)?;
        let content = tokio::fs::read_to_string(&path).await?;
        let matches = content.matches(old_string).count();
        if matches == 0 {
            return Ok(ToolOutput::error("old_string was not found"));
        }
        if matches > 1 && !replace_all {
            return Ok(ToolOutput::error(format!(
                "old_string matched {matches} times; provide more context or set replace_all"
            )));
        }
        let replaced = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        tokio::fs::write(&path, replaced).await?;
        Ok(ToolOutput::ok(format!(
            "replaced {matches} occurrence(s) in {}",
            path.display()
        )))
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

    #[tokio::test]
    async fn edits_files_with_exact_unique_matches() {
        let (_dir, context) = ctx();
        let edited = EditFile
            .execute(
                &context,
                json!({"path": "hello.txt", "old_string": "world", "new_string": "park"}),
            )
            .await
            .unwrap();
        assert!(!edited.is_error);
        let content = std::fs::read_to_string(context.root.join("hello.txt")).unwrap();
        assert_eq!(content, "hello\npark\n");

        std::fs::write(context.root.join("dup.txt"), "same same\n").unwrap();
        let ambiguous = EditFile
            .execute(
                &context,
                json!({"path": "dup.txt", "old_string": "same", "new_string": "x"}),
            )
            .await
            .unwrap();
        assert!(ambiguous.is_error);
        let replaced = EditFile
            .execute(
                &context,
                json!({
                    "path": "dup.txt",
                    "old_string": "same",
                    "new_string": "x",
                    "replace_all": true
                }),
            )
            .await
            .unwrap();
        assert!(!replaced.is_error);
        assert_eq!(
            std::fs::read_to_string(context.root.join("dup.txt")).unwrap(),
            "x x\n"
        );
    }

    #[test]
    fn blocks_private_and_special_ip_ranges() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            let ip = address.parse().unwrap();
            assert!(is_blocked_ip(ip), "{address} should be blocked");
        }
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            let ip = address.parse().unwrap();
            assert!(!is_blocked_ip(ip), "{address} should be public");
        }
        assert!(is_blocked_host("localhost"));
        assert!(is_blocked_host("printer.local."));
        assert!(is_blocked_host("metadata.internal"));
        assert!(!is_blocked_host("example.com"));
    }

    #[tokio::test]
    async fn rejects_hostnames_that_resolve_to_loopback() {
        assert!(resolve_public_address("localhost", 80).await.is_err());
    }

    #[tokio::test]
    async fn blocks_local_fetches() {
        let (_dir, context) = ctx();
        let output = FetchUrl
            .execute(&context, json!({"url": "http://127.0.0.1:1/"}))
            .await
            .unwrap();
        assert!(output.is_error);
        let output = FetchUrl
            .execute(&context, json!({"url": "file:///etc/passwd"}))
            .await
            .unwrap();
        assert!(output.is_error);
    }

    #[test]
    fn write_and_shell_require_approval() {
        assert!(EditFile.requires_approval());
        assert!(WriteFile.requires_approval());
        assert!(Shell.requires_approval());
        assert!(!ReadFile.requires_approval());
    }
}

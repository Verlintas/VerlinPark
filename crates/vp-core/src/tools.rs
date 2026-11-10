use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};

use crate::tooling::diff::render_diff;
use crate::tooling::fs::{atomic_write, sha256_hex, utf8_line_count};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;
use vp_providers::ToolSchema;

const MAX_TOOL_OUTPUT: usize = 40_000;
/// Files above this size are not loaded into memory by read_file. The agent can
/// use shell tools (head/sed/grep) for targeted reads instead.
const MAX_READ_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// grep skips very large files rather than allocating their full contents.
const MAX_GREP_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Per-stream cap for shell output. The process is not killed when it exceeds
/// the cap; the remainder is drained and discarded so a chatty build cannot
/// block on a full pipe or exhaust daemon memory.
const MAX_SHELL_STREAM_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub root: PathBuf,
    /// The repository the session was opened from. Used only to validate that
    /// shell Git metadata belongs to this project.
    pub project_root: PathBuf,
    /// Runtime-owned scratch space for shell caches and temporary files.
    pub sandbox_root: PathBuf,
    /// Credential/state paths that shell commands must not read.
    pub secret_roots: Vec<PathBuf>,
    /// Whether shell commands may open network connections.
    pub network_allowed: bool,
    /// The user's home directory, used by the optional home read jail.
    pub home: PathBuf,
    /// Deny reads under home except worktree/scratch/Git metadata/toolchains.
    pub restrict_home: bool,
    /// Deny reads under the home parent (normally `/Users`) and `/Volumes`
    /// outside the worktree/scratch/Git metadata/toolchain allowlist.
    pub read_jail: bool,
    /// The branch this agent is allowed to advance through Git metadata writes.
    /// `None` grants no common-ref writes.
    pub git_branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// Optional machine-readable result for the event stream and UI. The
    /// provider still receives `content`; tools must keep that rendering
    /// concise and actionable.
    pub structured: Option<Value>,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            structured: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            structured: None,
        }
    }

    pub fn ok_structured(content: impl Into<String>, structured: Value) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            structured: Some(structured),
        }
    }

    pub fn error_structured(content: impl Into<String>, structured: Value) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            structured: Some(structured),
        }
    }

    pub fn with_structured(mut self, structured: Value) -> Self {
        self.structured = Some(structured);
        self
    }
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
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
                Arc::new(crate::tooling::files::FindFiles),
                Arc::new(crate::tooling::files::FileInfo),
                Arc::new(crate::tooling::files::ReadFiles),
                Arc::new(crate::tooling::files::ApplyPatch),
                Arc::new(crate::tooling::files::FileManage),
                Arc::new(crate::tooling::project::ProjectProfile),
                Arc::new(crate::tooling::project::RunProjectTask),
                Arc::new(crate::tooling::git::GitQuery),
                Arc::new(crate::tooling::code::RepoMap),
                Arc::new(crate::tooling::code::FindSymbol),
                Arc::new(crate::tooling::code::FindReferences),
                Arc::new(crate::tooling::code::CodeDiagnostics),
                Arc::new(crate::tooling::code::AffectedTests),
                Arc::new(crate::tooling::code::DependencyGraph),
                Arc::new(crate::tooling::code::BuildContext),
                Arc::new(crate::tooling::process::ProcessTool),
                Arc::new(crate::tooling::process::WaitFor),
                Arc::new(crate::tooling::http::LocalHttp),
                Arc::new(crate::tooling::lsp::LspQuery),
                Arc::new(crate::tooling::web::WebSearch),
                Arc::new(crate::tooling::media::ReadPdf),
                Arc::new(crate::tooling::media::ReadImage),
                Arc::new(crate::tooling::data::DataQuery),
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

/// Write confinement for the shell tool.
///
/// The Seatbelt profile allows normal reads, process execution, and network
/// access, but denies file writes outside the agent worktree, its Git metadata,
/// and a runtime-owned scratch directory.
/// Git metadata must stay writable for `git add`/`commit`/`status`; this is the
/// one intentional exception to "writes stay inside the worktree".
const SHELL_SANDBOX_PROFILE: &str = r#"(version 1)
(allow default)
(deny file-write* (require-not (require-any
  (subpath (param "WORKTREE"))
  (subpath (param "SCRATCH"))
  (subpath (param "GIT_DIR"))
  (subpath (param "COMMON_OBJECTS"))
  (subpath (param "COMMON_LOGS"))
  (literal (param "BRANCH_REF"))
  (literal (param "BRANCH_REF_LOCK"))
  (literal "/dev/null")
)))
(deny file-write* (literal (param "WORKTREE_GITFILE")))
(deny file-write* (literal (param "GIT_HEAD")))
(deny file-write* (literal (param "GIT_COMMONDIR")))
(deny file-write* (literal (param "COMMON_PACKED_REFS")))
(deny file-write* (subpath (param "COMMON_OBJECTS_INFO")))
(deny file-write* (subpath (param "COMMON_OBJECTS_PACK")))
(deny file-write* (subpath (param "COMMON_HOOKS")))
(deny file-write* (literal (param "COMMON_CONFIG")))
(deny file-write* (literal (param "WORKTREE_CONFIG")))
(deny appleevent-send)
"#;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Variables that are safe and useful to pass into a build/test shell. Provider
/// keys, cloud credentials, tokens, and daemon configuration are deliberately
/// excluded; a command must set any extra project env explicitly.
pub(crate) const SHELL_ENV_PASSTHROUGH: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "CI",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "RUSTUP_HOME",
    "JAVA_HOME",
    "GOROOT",
    "GOPATH",
    "NVM_DIR",
    "NVM_BIN",
    "NVM_INC",
    "VOLTA_HOME",
    "ASDF_DIR",
    "ASDF_DATA_DIR",
    "PYENV_ROOT",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "CONDA_DEFAULT_ENV",
    "GEM_HOME",
    "GEM_PATH",
    "PNPM_HOME",
    "BUN_INSTALL",
    "HOMEBREW_PREFIX",
    "HOMEBREW_CELLAR",
    "HOMEBREW_REPOSITORY",
    "DEVELOPER_DIR",
    "SDKROOT",
    "MACOSX_DEPLOYMENT_TARGET",
    "CC",
    "CXX",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "PKG_CONFIG_PATH",
];

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
        || (segments[0] == 0x2001 && segments[1] == 0)
        || segments[0] == 0x2002
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[..6].iter().all(|segment| *segment == 0)
            && segments[6..].iter().any(|segment| *segment != 0))
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

#[derive(Debug, Clone)]
pub(crate) struct FetchedUrl {
    pub status: u16,
    pub content_type: String,
    pub text: String,
    pub truncated: bool,
    pub final_url: String,
    pub redirects: Vec<String>,
}

/// Fetch a public HTTP(S) URL with SSRF checks on every redirect hop. Callers
/// receive the decoded body and decide whether to render HTML or treat it as
/// text.
pub(crate) async fn fetch_public_url(
    raw_url: &str,
    max_chars: usize,
    max_bytes: usize,
) -> Result<FetchedUrl> {
    let max_chars = max_chars.clamp(100, 4_000_000);
    let max_bytes = max_bytes.clamp(1_024, 8 * 1024 * 1024);
    let mut current = reqwest::Url::parse(raw_url).context("invalid URL")?;
    let mut redirects = Vec::new();
    for hop in 0..=5 {
        validate_public_url(&current).await?;
        let host = current
            .host_str()
            .ok_or_else(|| anyhow!("URL has no host"))?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let port = current.port_or_known_default().unwrap_or(80);
        let mut client_builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .user_agent("VerlinPark/0.1 research fetcher");
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(ip) {
                bail!("private or local IP addresses are blocked");
            }
        } else {
            let address = resolve_public_address(&host, port).await?;
            client_builder = client_builder.resolve(&host, address);
        }
        let client = client_builder.build().context("building HTTP client")?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|error| anyhow!("fetch failed: {error}"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| anyhow!("redirect response has no Location header"))?;
            if hop >= 5 {
                bail!("too many redirects (max 5)");
            }
            let next = current
                .join(location)
                .map_err(|error| anyhow!("invalid redirect location: {error}"))?;
            redirects.push(next.to_string());
            current = next;
            continue;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let mut body = Vec::new();
        let mut response = response;
        let mut truncated = false;
        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() >= max_bytes {
                truncated = true;
                break;
            }
        }
        let text = String::from_utf8_lossy(&body).to_string();
        let mut chars = text.chars();
        let mut output = chars.by_ref().take(max_chars).collect::<String>();
        if chars.next().is_some() {
            truncated = true;
            output.push_str("\n… content truncated");
        }
        return Ok(FetchedUrl {
            status: status.as_u16(),
            content_type,
            text: output,
            truncated,
            final_url: current.to_string(),
            redirects,
        });
    }
    bail!("too many redirects (max 5)")
}

async fn validate_public_url(url: &reqwest::Url) -> Result<()> {
    match url.scheme() {
        "http" | "https" => {}
        other => bail!("unsupported URL scheme: {other}"),
    }
    if url.username() != "" || url.password().is_some() {
        bail!("URL credentials are not allowed");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("URL has no host"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if is_blocked_host(&host) {
        bail!("local hosts are blocked");
    }
    if let Ok(ip) = host.parse::<IpAddr>()
        && is_blocked_ip(ip)
    {
        bail!("private or local IP addresses are blocked");
    }
    Ok(())
}

fn html_title(html: &str) -> Option<String> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let regex = TITLE.get_or_init(|| Regex::new(r"(?is)<title\b[^>]*>(.*?)</title\s*>").unwrap());
    let raw = regex.captures(html)?.get(1)?.as_str();
    let text = crate::sources::html_to_text(raw);
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(300).collect())
}

fn html_links(html: &str, base: &reqwest::Url) -> Vec<String> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let regex = LINK.get_or_init(|| Regex::new(r#"(?is)href\s*=\s*["']([^"']+)["']"#).unwrap());
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for captures in regex.captures_iter(html) {
        let Some(href) = captures.get(1).map(|value| value.as_str()) else {
            continue;
        };
        if href.starts_with('#') || href.starts_with("javascript:") || href.starts_with("mailto:") {
            continue;
        }
        let Ok(url) = base.join(href) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https")
            || url.username() != ""
            || url.password().is_some()
        {
            continue;
        }
        let value = url.to_string();
        if seen.insert(value.clone()) {
            links.push(value);
            if links.len() >= 50 {
                break;
            }
        }
    }
    links
}

#[async_trait]
impl AgentTool for FetchUrl {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn description(&self) -> &'static str {
        "Fetch a public HTTP(S) URL with redirects and SSRF protection. HTML is reduced to readable text with a title and links."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Public http(s) URL"},
                "max_chars": {"type": "integer", "minimum": 100, "maximum": 20000},
                "include_links": {"type": "boolean", "description": "Return extracted links; default true"}
            },
            "required": ["url"]
        })
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
        let include_links = input
            .get("include_links")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        match fetch_public_url(raw_url, max_chars, max_chars.saturating_mul(4)).await {
            Ok(fetched) => {
                let content_type = fetched.content_type.to_ascii_lowercase();
                if content_type.starts_with("image/")
                    || content_type.starts_with("audio/")
                    || content_type.starts_with("video/")
                    || content_type.contains("pdf")
                    || content_type.contains("octet-stream")
                {
                    return Ok(ToolOutput::error_structured(
                        format!("binary content is not readable as text: {content_type}"),
                        json!({
                            "code": "UNSUPPORTED_CONTENT",
                            "content_type": fetched.content_type,
                            "final_url": fetched.final_url,
                        }),
                    ));
                }
                let is_html = content_type.contains("html")
                    || fetched.text.trim_start().starts_with("<!doctype")
                    || fetched.text.trim_start().starts_with("<html");
                let title = is_html.then(|| html_title(&fetched.text)).flatten();
                let links = if is_html && include_links {
                    html_links(
                        &fetched.text,
                        &reqwest::Url::parse(&fetched.final_url)
                            .unwrap_or_else(|_| reqwest::Url::parse("https://invalid").unwrap()),
                    )
                } else {
                    Vec::new()
                };
                let content = if is_html {
                    crate::sources::html_to_text(&fetched.text)
                } else {
                    fetched.text.clone()
                };
                let mut content_chars = content.chars();
                let mut visible = content_chars.by_ref().take(max_chars).collect::<String>();
                if content_chars.next().is_some() {
                    visible.push_str("\n… content truncated");
                }
                let hash = sha256_hex(fetched.text.as_bytes());
                let header = format!(
                    "GET {raw_url} -> {} ({}){}{}",
                    fetched.status,
                    fetched.content_type,
                    if fetched.redirects.is_empty() {
                        String::new()
                    } else {
                        format!(" · {} redirect(s)", fetched.redirects.len())
                    },
                    title
                        .as_ref()
                        .map(|title| format!(" · {title}"))
                        .unwrap_or_default()
                );
                Ok(ToolOutput::ok_structured(
                    truncate(format!("{header}\n{visible}")),
                    json!({
                        "ok": fetched.status < 400,
                        "status": fetched.status,
                        "content_type": fetched.content_type,
                        "final_url": fetched.final_url,
                        "redirects": fetched.redirects,
                        "title": title,
                        "links": links,
                        "text": visible,
                        "sha256": hash,
                        "truncated": fetched.truncated,
                    }),
                ))
            }
            Err(error) => Ok(ToolOutput::error_structured(
                error.to_string(),
                json!({"code": "FETCH_ERROR", "url": raw_url}),
            )),
        }
    }
}

#[async_trait]
impl AgentTool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a UTF-8 text file from the project. Returns line-numbered content and a content hash for stale-read checks."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the worktree root"},
                "offset": {"type": "integer", "minimum": 0, "description": "Zero-based starting line"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Maximum lines"}
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(400) as usize;
        let path = safe_existing_path(&context.root, path)?;
        let metadata = tokio::fs::metadata(&path).await?;
        if metadata.len() > MAX_READ_FILE_BYTES {
            return Ok(ToolOutput::error_structured(
                format!(
                    "file is {} bytes; read_file is limited to {} bytes. Use a bounded offset read or shell sed/head.",
                    metadata.len(),
                    MAX_READ_FILE_BYTES
                ),
                json!({
                    "code": "FILE_TOO_LARGE",
                    "path": path,
                    "size_bytes": metadata.len(),
                    "limit_bytes": MAX_READ_FILE_BYTES,
                }),
            ));
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let total_lines = content.lines().count();
        let lines = content
            .lines()
            .skip(offset)
            .take(limit)
            .enumerate()
            .map(|(index, line)| format!("{:>5} | {}", offset + index + 1, line))
            .collect::<Vec<_>>();
        let returned = lines.len();
        let truncated = offset > 0 || offset.saturating_add(returned) < total_lines;
        let hash = sha256_hex(content.as_bytes());
        let mut out = format!(
            "{} ({} lines, sha256 {})\n",
            path.display(),
            total_lines,
            hash
        );
        out.push_str(&lines.join("\n"));
        if truncated {
            out.push('\n');
            out.push_str(&format!(
                "… showing lines {}-{} of {}; next_offset={}",
                offset.saturating_add(1),
                offset.saturating_add(returned),
                total_lines,
                offset.saturating_add(returned)
            ));
        }
        Ok(ToolOutput::ok_structured(
            truncate(out),
            json!({
                "path": path,
                "sha256": hash,
                "total_lines": total_lines,
                "offset": offset,
                "lines": returned,
                "truncated": truncated,
                "next_offset": truncated.then_some(offset.saturating_add(returned)),
                "encoding": "utf-8",
            }),
        ))
    }
}

#[async_trait]
impl AgentTool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List files and directories under a project-relative path. One level only; use find_files for recursive discovery."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the worktree root; default ."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "Maximum entries; default 500"},
                "cursor": {"type": "integer", "minimum": 0, "description": "Zero-based entry offset for pagination"},
                "include_hidden": {"type": "boolean", "description": "Include dotfiles; default true"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let include_hidden = input
            .get("include_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(500)
            .clamp(1, 2_000) as usize;
        let cursor = input.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize;
        let path = safe_existing_path(&context.root, relative)?;
        let mut entries = Vec::new();
        let mut reader = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = reader.next_entry().await? {
            let metadata = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            entries.push((name, metadata.is_dir(), metadata.len()));
        }
        entries.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.to_lowercase().cmp(&right.0.to_lowercase()))
        });
        let total = entries.len();
        let page = entries
            .iter()
            .skip(cursor)
            .take(limit)
            .map(|(name, is_dir, size)| {
                json!({
                    "name": name,
                    "type": if *is_dir { "dir" } else { "file" },
                    "size_bytes": size,
                })
            })
            .collect::<Vec<_>>();
        let lines = page
            .iter()
            .map(|entry| {
                let name = entry["name"].as_str().unwrap_or_default();
                if entry["type"] == "dir" {
                    format!("{name}/")
                } else {
                    format!(
                        "{name} ({} bytes)",
                        entry["size_bytes"].as_u64().unwrap_or(0)
                    )
                }
            })
            .collect::<Vec<_>>();
        let next_cursor = cursor.saturating_add(page.len());
        let truncated = next_cursor < total;
        let mut rendered = lines.join("\n");
        if truncated || cursor > 0 {
            rendered.push_str(&format!(
                "\n… showing entries {}-{} of {}; next_cursor={}",
                cursor.saturating_add(1),
                next_cursor,
                total,
                next_cursor
            ));
        }
        Ok(ToolOutput::ok_structured(
            truncate(rendered),
            json!({
                "path": path,
                "entries": page,
                "total": total,
                "cursor": cursor,
                "next_cursor": truncated.then_some(next_cursor),
                "truncated": truncated,
            }),
        ))
    }
}

#[async_trait]
impl AgentTool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search project files with a regular expression. Returns path:line matches plus machine-readable context."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Rust regex pattern"},
                "path": {"type": "string", "description": "Directory or file relative to the worktree root; default ."},
                "glob": {"type": "string", "description": "Optional glob such as **/*.rs"},
                "case_sensitive": {"type": "boolean", "description": "Default true"},
                "context": {"type": "integer", "minimum": 0, "maximum": 5, "description": "Context lines around each match"},
                "path_only": {"type": "boolean", "description": "Return one path per matching file"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500}
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let pattern = input
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("pattern is required"))?;
        let case_sensitive = input
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let regex = RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .context("invalid regex")?;
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let search_path = safe_existing_path(&context.root, relative)?;
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let context_lines = input
            .get("context")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(5) as usize;
        let path_only = input
            .get("path_only")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let globset = input
            .get("glob")
            .and_then(Value::as_str)
            .map(build_globset)
            .transpose()?;

        let mut matches = Vec::new();
        let mut seen_paths = std::collections::HashSet::new();
        let mut files_scanned = 0usize;
        let mut truncated = false;
        let mut builder = WalkBuilder::new(&search_path);
        builder.hidden(false).git_ignore(true).require_git(false);
        'files: for entry in builder.build().flatten() {
            if matches.len() >= limit {
                truncated = true;
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
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            if metadata.len() > MAX_GREP_FILE_BYTES {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            if content.contains('\0') {
                continue;
            }
            files_scanned = files_scanned.saturating_add(1);
            let lines = content.lines().collect::<Vec<_>>();
            let relative_path = path
                .strip_prefix(&context.root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            for (line_index, line) in lines.iter().enumerate() {
                if !regex.is_match(line) {
                    continue;
                }
                if path_only {
                    if seen_paths.insert(relative_path.clone()) {
                        matches.push(json!({
                            "path": relative_path,
                            "line": line_index + 1,
                            "text": line,
                        }));
                    }
                    if matches.len() >= limit {
                        truncated = true;
                        break 'files;
                    }
                    continue;
                }
                let start = line_index.saturating_sub(context_lines);
                let end = (line_index + context_lines + 1).min(lines.len());
                let before = lines[start..line_index].to_vec();
                let after = lines[line_index + 1..end].to_vec();
                matches.push(json!({
                    "path": relative_path,
                    "line": line_index + 1,
                    "text": line,
                    "context_before": before,
                    "context_after": after,
                }));
                if matches.len() >= limit {
                    truncated = true;
                    break 'files;
                }
            }
        }
        let rendered = if path_only {
            let mut paths = matches
                .iter()
                .filter_map(|entry| entry["path"].as_str())
                .map(str::to_string)
                .collect::<Vec<_>>();
            paths.dedup();
            paths.join("\n")
        } else {
            matches
                .iter()
                .map(|entry| {
                    let mut block = String::new();
                    if let Some(before) = entry["context_before"].as_array() {
                        for line in before {
                            block.push_str(&format!("  {}\n", line.as_str().unwrap_or("")));
                        }
                    }
                    block.push_str(&format!(
                        "{}:{}: {}",
                        entry["path"].as_str().unwrap_or("?"),
                        entry["line"].as_u64().unwrap_or(0),
                        entry["text"].as_str().unwrap_or("")
                    ));
                    if let Some(after) = entry["context_after"].as_array() {
                        for line in after {
                            block.push_str(&format!("\n  {}", line.as_str().unwrap_or("")));
                        }
                    }
                    block
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ToolOutput::ok_structured(
            truncate(rendered),
            json!({
                "matches": matches,
                "match_count": matches.len(),
                "files_scanned": files_scanned,
                "truncated": truncated,
            }),
        ))
    }
}

#[async_trait]
impl AgentTool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact string in an existing UTF-8 file atomically. Returns a bounded diff and content hashes."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the worktree root"},
                "old_string": {"type": "string", "description": "Exact text to replace"},
                "new_string": {"type": "string", "description": "Replacement text"},
                "replace_all": {"type": "boolean", "description": "Replace every match; default false"},
                "expected_sha256": {"type": "string", "description": "Reject the edit if the current file hash differs"},
                "dry_run": {"type": "boolean", "description": "Validate and return a diff without writing"}
            },
            "required": ["path", "old_string", "new_string"]
        })
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
        let dry_run = input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if old_string.is_empty() {
            return Ok(ToolOutput::error_structured(
                "old_string must not be empty",
                json!({"code": "INVALID_ARGUMENT", "path": path}),
            ));
        }
        let path = safe_existing_path(&context.root, path)?;
        if path_is_git_metadata(&context.root, &path) {
            return Ok(ToolOutput::error_structured(
                "refusing to edit Git metadata through a file tool",
                json!({"code": "PATH_ESCAPE", "path": path}),
            ));
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let before_hash = sha256_hex(content.as_bytes());
        if let Some(expected) = input.get("expected_sha256").and_then(Value::as_str)
            && expected != before_hash
        {
            return Ok(ToolOutput::error_structured(
                "file hash changed since it was read",
                json!({
                    "code": "HASH_MISMATCH",
                    "path": path,
                    "expected_sha256": expected,
                    "actual_sha256": before_hash,
                }),
            ));
        }
        let matches = content.matches(old_string).count();
        if matches == 0 {
            return Ok(ToolOutput::error_structured(
                "old_string was not found",
                json!({"code": "PATCH_CONFLICT", "path": path}),
            ));
        }
        if matches > 1 && !replace_all {
            return Ok(ToolOutput::error_structured(
                format!(
                    "old_string matched {matches} times; provide more context or set replace_all"
                ),
                json!({
                    "code": "PATCH_CONFLICT",
                    "path": path,
                    "matches": matches,
                }),
            ));
        }
        let replaced = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        let after_hash = sha256_hex(replaced.as_bytes());
        let diff = render_diff(&content, &replaced, 3);
        if !dry_run {
            atomic_write(&path, replaced.as_bytes())?;
            crate::tooling::code::invalidate_index(&context.root);
        }
        let content_text = format!(
            "{} {matches} occurrence(s) in {}\n{}",
            if dry_run { "would replace" } else { "replaced" },
            path.display(),
            diff
        );
        Ok(ToolOutput::ok_structured(
            truncate(content_text),
            json!({
                "path": path,
                "replacements": matches,
                "dry_run": dry_run,
                "before_sha256": before_hash,
                "after_sha256": after_hash,
                "diff": diff,
                "bytes": replaced.len(),
            }),
        ))
    }
}

#[async_trait]
impl AgentTool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create or overwrite a UTF-8 file atomically in the worktree. Returns the content hash and a bounded diff when replacing a file."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path relative to the worktree root"},
                "content": {"type": "string", "description": "Full file content"},
                "expected_sha256": {"type": "string", "description": "Reject the write if the current file hash differs"},
                "dry_run": {"type": "boolean", "description": "Validate and return a diff without writing"}
            },
            "required": ["path", "content"]
        })
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
        let dry_run = input
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let path = safe_write_path(&context.root, path)?;
        let old_content = match tokio::fs::read_to_string(&path).await {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut before_hash = None;
        if let Some(old) = &old_content {
            let hash = sha256_hex(old.as_bytes());
            before_hash = Some(hash.clone());
            if let Some(expected) = input.get("expected_sha256").and_then(Value::as_str)
                && expected != hash
            {
                return Ok(ToolOutput::error_structured(
                    "file hash changed since it was read",
                    json!({
                        "code": "HASH_MISMATCH",
                        "path": path,
                        "expected_sha256": expected,
                        "actual_sha256": hash,
                    }),
                ));
            }
        } else if input.get("expected_sha256").is_some() {
            return Ok(ToolOutput::error_structured(
                "expected_sha256 was provided but the file does not exist",
                json!({"code": "HASH_MISMATCH", "path": path}),
            ));
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if !dry_run {
            atomic_write(&path, content.as_bytes())?;
            crate::tooling::code::invalidate_index(&context.root);
        }
        let after_hash = sha256_hex(content.as_bytes());
        let diff = old_content
            .as_deref()
            .map(|old| render_diff(old, content, 3))
            .unwrap_or_default();
        let created = old_content.is_none();
        let content_text = format!(
            "{} {} bytes to {}\n{}",
            if dry_run { "would write" } else { "wrote" },
            content.len(),
            path.display(),
            diff
        );
        Ok(ToolOutput::ok_structured(
            truncate(content_text),
            json!({
                "path": path,
                "bytes": content.len(),
                "created": created,
                "dry_run": dry_run,
                "sha256": after_hash,
                "before_sha256": before_hash,
                "diff": diff,
                "line_count": utf8_line_count(content),
            }),
        ))
    }
}

pub(crate) async fn sandboxed_command(context: &ToolContext) -> Result<Command> {
    if !Path::new(SANDBOX_EXEC).is_file() {
        bail!("shell sandbox is unavailable on this platform; refusing to run unsandboxed shell");
    }
    let root = context.root.canonicalize().context("resolving worktree")?;
    tokio::fs::create_dir_all(&context.sandbox_root).await?;
    let scratch = context
        .sandbox_root
        .canonicalize()
        .context("resolving shell scratch directory")?;
    let tmp = scratch.join("tmp");
    tokio::fs::create_dir_all(&tmp).await?;
    let project_root = context
        .project_root
        .canonicalize()
        .unwrap_or_else(|_| root.clone());
    let (git_dir, common_dir) = git_metadata_dirs(&root, &project_root)?;
    let mut profile = String::from(SHELL_SANDBOX_PROFILE);
    if !context.network_allowed {
        profile.push_str("(deny network*)\n");
    }
    let secret_roots = context
        .secret_roots
        .iter()
        .map(|secret| secret.canonicalize().unwrap_or_else(|_| secret.clone()))
        .collect::<Vec<_>>();
    let mut allowed_paths = Vec::new();
    if context.restrict_home || context.read_jail {
        let home = context
            .home
            .canonicalize()
            .unwrap_or_else(|_| context.home.clone());
        if context.restrict_home {
            profile.push_str("(deny file-read* (subpath (param \"HOME\")))\n");
        }
        if context.read_jail {
            if let Some(parent) = home.parent()
                && parent != Path::new("/")
            {
                profile.push_str("(deny file-read* (subpath (param \"READ_ROOT\")))\n");
            }
            profile.push_str("(deny file-read* (subpath \"/Volumes\"))\n");
        }
        if context.restrict_home {
            profile.push_str("(allow file-read-metadata (subpath (param \"HOME\")))\n");
        }
        profile.push_str(
                "(allow file-read* (subpath (param \"WORKTREE\")) (subpath (param \"SCRATCH\")) (subpath (param \"GIT_DIR\")) (subpath (param \"COMMON_DIR\")))\n",
            );
        let mut candidates = vec![
            home.join(".cargo"),
            home.join(".rustup"),
            home.join(".nvm"),
            home.join(".volta"),
            home.join(".asdf"),
            home.join(".pyenv"),
            home.join(".bun"),
            home.join(".local"),
            home.join("Library/Developer"),
        ];
        for key in [
            "RUSTUP_HOME",
            "CARGO_HOME",
            "NVM_DIR",
            "VOLTA_HOME",
            "ASDF_DIR",
            "ASDF_DATA_DIR",
            "PYENV_ROOT",
            "VIRTUAL_ENV",
            "CONDA_PREFIX",
            "GEM_HOME",
            "GEM_PATH",
            "PNPM_HOME",
            "BUN_INSTALL",
            "HOMEBREW_PREFIX",
            "HOMEBREW_CELLAR",
            "HOMEBREW_REPOSITORY",
            "JAVA_HOME",
            "GOROOT",
            "GOPATH",
            "DEVELOPER_DIR",
            "SDKROOT",
        ] {
            if let Ok(value) = std::env::var(key) {
                for part in value.split(':') {
                    if !part.is_empty() {
                        candidates.push(PathBuf::from(part));
                    }
                }
            }
        }
        for candidate in candidates {
            let candidate = candidate.canonicalize().unwrap_or(candidate);
            if candidate.is_dir() && !allowed_paths.contains(&candidate) {
                allowed_paths.push(candidate);
            }
        }
        for path in [
            home.join(".gitconfig"),
            home.join(".ssh/config"),
            home.join(".ssh/known_hosts"),
        ] {
            if path.is_file() && !allowed_paths.contains(&path) {
                allowed_paths.push(path);
            }
        }
        for (index, path) in allowed_paths.iter().enumerate() {
            let rule = if path.is_dir() { "subpath" } else { "literal" };
            profile.push_str(&format!(
                "(allow file-read* ({rule} (param \"ALLOW{index}\")))\n"
            ));
        }
    }
    for (index, _) in secret_roots.iter().enumerate() {
        profile.push_str(&format!(
            "(deny file-read* (subpath (param \"SECRET{index}\")))\n"
        ));
    }
    let mut process = Command::new(SANDBOX_EXEC);
    process.env_clear();
    for key in SHELL_ENV_PASSTHROUGH {
        if let Some(value) = std::env::var_os(key) {
            process.env(key, value);
        }
    }
    let branch = context
        .git_branch
        .as_deref()
        .filter(|branch| !branch.trim().is_empty())
        .unwrap_or("__no_branch__");
    let branch_ref = common_dir.join("refs").join("heads").join(branch);
    let branch_ref_lock = PathBuf::from(format!("{}.lock", branch_ref.display()));
    let common_objects = common_dir.join("objects");
    let worktree_gitfile = root.join(".git");
    let params = [
        ("WORKTREE", root.clone()),
        ("SCRATCH", scratch.clone()),
        ("GIT_DIR", git_dir.clone()),
        ("COMMON_DIR", common_dir.clone()),
        ("COMMON_OBJECTS", common_objects.clone()),
        ("COMMON_OBJECTS_INFO", common_objects.join("info")),
        ("COMMON_OBJECTS_PACK", common_objects.join("pack")),
        ("COMMON_LOGS", common_dir.join("logs")),
        ("COMMON_HOOKS", common_dir.join("hooks")),
        ("COMMON_CONFIG", common_dir.join("config")),
        ("COMMON_PACKED_REFS", common_dir.join("packed-refs")),
        ("BRANCH_REF", branch_ref.clone()),
        ("BRANCH_REF_LOCK", branch_ref_lock),
        ("WORKTREE_GITFILE", worktree_gitfile),
        ("GIT_HEAD", git_dir.join("HEAD")),
        ("GIT_COMMONDIR", git_dir.join("commondir")),
        ("WORKTREE_CONFIG", git_dir.join("config.worktree")),
    ];
    for (name, value) in params {
        process.arg("-D").arg(format!("{name}={}", value.display()));
    }
    for (index, secret) in secret_roots.iter().enumerate() {
        process
            .arg("-D")
            .arg(format!("SECRET{index}={}", secret.display()));
    }
    if context.restrict_home || context.read_jail {
        let home = context
            .home
            .canonicalize()
            .unwrap_or_else(|_| context.home.clone());
        process.arg("-D").arg(format!("HOME={}", home.display()));
        if context.read_jail
            && let Some(parent) = home.parent()
            && parent != Path::new("/")
        {
            process
                .arg("-D")
                .arg(format!("READ_ROOT={}", parent.display()));
        }
        for (index, path) in allowed_paths.iter().enumerate() {
            process
                .arg("-D")
                .arg(format!("ALLOW{index}={}", path.display()));
        }
    }
    process
        .arg("-p")
        .arg(&profile)
        .current_dir(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("TMPDIR", &tmp)
        .env("TMP", &tmp)
        .env("TEMP", &tmp)
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .env("CARGO_HOME", scratch.join("cargo"))
        .env("GOCACHE", scratch.join("go-build"))
        .env("GOMODCACHE", scratch.join("go-mod"))
        .env("npm_config_cache", scratch.join("npm"))
        .env("YARN_CACHE_FOLDER", scratch.join("yarn"))
        .env("PIP_CACHE_DIR", scratch.join("pip"))
        .env("CLANG_MODULE_CACHE_PATH", scratch.join("clang-modules"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "gc.auto")
        .env("GIT_CONFIG_VALUE_0", "0")
        .kill_on_drop(true);
    if context.restrict_home || context.read_jail {
        process
            .env("HOME", &context.home)
            .env("XDG_CONFIG_HOME", scratch.join("config"))
            .env("XDG_DATA_HOME", scratch.join("data"));
    }
    #[cfg(unix)]
    process.process_group(0);

    Ok(process)
}

#[async_trait]
impl AgentTool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Run a non-interactive shell command in the agent worktree. Writes are confined to the worktree, its Git metadata, and runtime scratch; host-level commands may require permission."
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

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("command is required"))?;
        if !Path::new(SANDBOX_EXEC).is_file() {
            return Ok(ToolOutput::error(
                "shell sandbox is unavailable on this platform; refusing to run unsandboxed shell",
            ));
        }
        let timeout_secs = input
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(120)
            .min(900);
        let started = std::time::Instant::now();
        let captured =
            match run_sandboxed_shell_captured(context, command, Duration::from_secs(timeout_secs))
                .await
            {
                Ok(captured) => captured,
                Err(error) => {
                    return Ok(ToolOutput::error_structured(
                        error.to_string(),
                        json!({
                            "code": if error.to_string().contains("timed out") {
                                "COMMAND_TIMEOUT"
                            } else {
                                "COMMAND_FAILED"
                            },
                            "command": command,
                            "elapsed_ms": started.elapsed().as_millis() as u64,
                        }),
                    ));
                }
            };
        crate::tooling::code::invalidate_index(&context.root);
        let mut warnings = Vec::new();
        if captured.stdout_truncated {
            warnings.push("stdout capped at 1 MiB");
        }
        if captured.stderr_truncated {
            warnings.push("stderr capped at 1 MiB");
        }
        let mut text = format!("exit {}\n", captured.exit_code);
        if !warnings.is_empty() {
            text.push_str(&format!("[{}]\n", warnings.join("; ")));
        }
        if !captured.stdout.is_empty() {
            text.push_str(&captured.stdout);
            if !captured.stdout.ends_with('\n') {
                text.push('\n');
            }
        }
        if !captured.stderr.is_empty() {
            text.push_str("[stderr]\n");
            text.push_str(&captured.stderr);
            if !captured.stderr.ends_with('\n') {
                text.push('\n');
            }
        }
        let structured = json!({
            "ok": captured.exit_code == 0,
            "command": command,
            "exit_code": captured.exit_code,
            "stdout": captured.stdout,
            "stderr": captured.stderr,
            "stdout_truncated": captured.stdout_truncated,
            "stderr_truncated": captured.stderr_truncated,
            "elapsed_ms": captured.duration_ms,
            "warnings": warnings,
        });
        let text = truncate(text);
        if captured.exit_code == 0 {
            Ok(ToolOutput::ok_structured(text, structured))
        } else {
            Ok(ToolOutput::error_structured(text, structured))
        }
    }
}

/// Structured shell capture used by the shell and project-task tools.
#[derive(Debug, Clone)]
pub(crate) struct ShellCapture {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub duration_ms: u64,
}

pub(crate) async fn run_sandboxed_shell_captured(
    context: &ToolContext,
    command: &str,
    timeout_duration: Duration,
) -> Result<ShellCapture> {
    run_sandboxed_shell_with_captured(context, "/bin/sh", &["-c"], command, timeout_duration).await
}

/// Login-shell variant used by the Integrator command for compatibility with
/// project setup scripts. It still runs under the same Seatbelt profile and
/// environment allowlist.
pub(crate) async fn run_sandboxed_login_shell(
    context: &ToolContext,
    command: &str,
    timeout_duration: Duration,
) -> Result<(i32, String)> {
    let captured =
        run_sandboxed_shell_with_captured(context, "/bin/sh", &["-lc"], command, timeout_duration)
            .await?;
    let mut text = captured.stdout;
    if !captured.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&captured.stderr);
    }
    Ok((captured.exit_code, text))
}

async fn run_sandboxed_shell_with_captured(
    context: &ToolContext,
    program: &str,
    args: &[&str],
    command: &str,
    timeout_duration: Duration,
) -> Result<ShellCapture> {
    let started = std::time::Instant::now();
    let mut process = sandboxed_command(context).await?;
    process
        .arg(program)
        .args(args)
        .arg(command)
        .stdin(Stdio::null());
    let mut child = process.spawn().context("spawning sandboxed shell")?;
    let _group = ProcessGroupGuard::new(child.id());
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("shell stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("shell stderr pipe is unavailable"))?;
    let stdout_task = tokio::spawn(read_capped_stream(stdout));
    let stderr_task = tokio::spawn(read_capped_stream(stderr));
    let status = match timeout(timeout_duration, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            // Do not await the readers: a background process may still hold
            // the pipe open, and waiting would delay process-group cleanup.
            stdout_task.abort();
            stderr_task.abort();
            return Err(anyhow!(
                "command timed out after {}s",
                timeout_duration.as_secs()
            ));
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| anyhow!("shell stdout reader failed: {error}"))?;
    let stderr = stderr_task
        .await
        .map_err(|error| anyhow!("shell stderr reader failed: {error}"))?;
    Ok(ShellCapture {
        exit_code: status.code().unwrap_or(-1),
        stdout: stdout.text,
        stderr: stderr.text,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

pub(crate) struct CappedStream {
    text: String,
    truncated: bool,
}

pub(crate) async fn read_capped_stream<R>(mut reader: R) -> CappedStream
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8192];
    let mut bytes = Vec::new();
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let remaining = MAX_SHELL_STREAM_BYTES.saturating_sub(bytes.len());
                if remaining > 0 {
                    bytes.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                if read > remaining {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    CappedStream {
        text: String::from_utf8_lossy(&bytes).to_string(),
        truncated,
    }
}

/// Kills the command's process group when dropped. `kill_on_drop` only kills the
/// direct sandbox-exec child; a shell can spawn background processes that would
/// otherwise survive cancellation or timeout.
struct ProcessGroupGuard(Option<i32>);

impl ProcessGroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self(pid.and_then(|pid| i32::try_from(pid).ok()))
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: kill(2) with a negative pid targets the process group we
            // created with process_group(0). ESRCH is the expected no-op when
            // the group has already exited.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

/// Return the worktree-specific Git directory and the shared common directory.
/// Linked worktrees keep only a `.git` file in the checkout; both paths are
/// needed so normal Git writes (index, refs, objects, logs) keep working.
fn git_metadata_dirs(root: &Path, project_root: &Path) -> Result<(PathBuf, PathBuf)> {
    // A checked-out verification clone (or any standalone repository) owns its
    // own `.git` directory. It must never fall back to the source project's Git
    // metadata, otherwise a supposedly read-only shell can rewrite source refs.
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        let git_dir = dot_git
            .canonicalize()
            .with_context(|| format!("resolving Git directory {}", dot_git.display()))?;
        let common_dir = resolve_common_dir(&git_dir);
        return Ok((git_dir, common_dir));
    }
    if !dot_git.exists() {
        // A probe/scratch directory that is not a repository. It gets a
        // non-existent Git parameter so the sandbox grants no project metadata.
        let placeholder = root.join(".git");
        return Ok((placeholder.clone(), placeholder));
    }

    let project_git = resolve_git_dir(project_root).unwrap_or_else(|| project_root.join(".git"));
    let resolved_common = resolve_common_dir(&project_git);
    let project_common = if is_git_dir(&resolved_common) {
        resolved_common
    } else {
        project_root.join(".git")
    };
    let worktrees_root = project_common.join("worktrees");
    let Some(git_dir) = resolve_git_dir(root) else {
        return Err(anyhow!(
            "refusing to run shell with an invalid Git pointer in {}",
            root.display()
        ));
    };
    if !git_dir.starts_with(&worktrees_root) {
        return Err(anyhow!(
            "refusing to run shell with Git metadata outside {}",
            worktrees_root.display()
        ));
    }
    let common_dir = resolve_common_dir(&git_dir);
    if common_dir != project_common {
        return Err(anyhow!(
            "refusing to run shell with unexpected Git common directory {}",
            common_dir.display()
        ));
    }
    Ok((git_dir, common_dir))
}

fn is_git_dir(path: &Path) -> bool {
    path.is_dir() && path.join("HEAD").is_file() && path.join("objects").is_dir()
}

fn resolve_git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return dot_git.canonicalize().ok();
    }
    let content = std::fs::read_to_string(&dot_git).ok()?;
    let path = content.lines().next()?.strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    path.canonicalize().ok()
}

fn resolve_common_dir(git_dir: &Path) -> PathBuf {
    std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .and_then(|content| {
            let path = PathBuf::from(content.trim());
            let path = if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            };
            path.canonicalize().ok()
        })
        .unwrap_or_else(|| git_dir.to_path_buf())
}

pub(crate) fn build_globset(pattern: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(pattern)?);
    Ok(builder.build()?)
}

pub(crate) fn safe_existing_path(root: &Path, relative: &str) -> Result<PathBuf> {
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

pub(crate) fn safe_write_path(root: &Path, relative: &str) -> Result<PathBuf> {
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
    if path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".git")
    }) {
        return Err(anyhow!(
            "refusing to write Git metadata through a file tool"
        ));
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

pub(crate) fn path_is_git_metadata(root: &Path, path: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        })
}

/// Cap machine-readable tool payloads before they enter the event stream. The
/// provider-facing text is already bounded; this prevents a verbose structured
/// result from bloating persisted events.
pub(crate) fn bounded_structured(value: Option<Value>) -> Option<Value> {
    const MAX_STRUCTURED_BYTES: usize = 32 * 1024;
    let value = value?;
    let text = serde_json::to_string(&value).ok()?;
    if text.len() <= MAX_STRUCTURED_BYTES {
        return Some(value);
    }
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("structured result exceeded the event cap");
    Some(json!({
        "truncated": true,
        "summary": summary,
        "original_bytes": text.len(),
        "note": "full result is available in the tool text; structured data was capped at 32 KiB",
    }))
}

pub(crate) fn truncate(mut content: String) -> String {
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
        let sandbox_root = dir.path().join(".sandbox");
        let home = dir.path().join("home");
        (
            dir,
            ToolContext {
                root: root.clone(),
                project_root: root,
                sandbox_root,
                home,
                restrict_home: false,
                read_jail: false,
                git_branch: None,
                secret_roots: Vec::new(),
                network_allowed: true,
            },
        )
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

    #[test]
    fn registry_tool_names_are_unique_and_cover_the_tooling_wave() {
        let registry = ToolRegistry::default_tools();
        let names = registry
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect::<Vec<_>>();
        let unique = names.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(names.len(), unique.len(), "duplicate tool names: {names:?}");
        for expected in [
            "apply_patch",
            "find_files",
            "project_profile",
            "run_project_task",
            "git",
            "repo_map",
            "lsp_query",
            "web_search",
            "read_pdf",
            "read_image",
            "data_query",
            "process",
            "local_http",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn extracts_html_title_and_links() {
        let html = r##"<html><head><title>Example &amp; Docs</title></head><body>
            <a href="/guide">Guide</a><a href="https://example.com/x">X</a>
            <a href="#local">skip</a></body></html>"##;
        assert_eq!(html_title(html).as_deref(), Some("Example & Docs"));
        let base = reqwest::Url::parse("https://example.com/docs/").unwrap();
        let links = html_links(html, &base);
        assert!(links.contains(&"https://example.com/guide".to_string()));
        assert!(links.contains(&"https://example.com/x".to_string()));
        assert!(!links.iter().any(|link| link.contains("#local")));
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
    async fn refuses_to_write_git_metadata_through_file_tools() {
        let (_dir, context) = ctx();
        std::fs::create_dir_all(context.root.join(".git")).unwrap();
        std::fs::write(context.root.join(".git/config"), "[core]\n").unwrap();
        let write = WriteFile
            .execute(&context, json!({"path": ".git/config", "content": "x"}))
            .await;
        assert!(write.is_err());
        let case_variant = WriteFile
            .execute(&context, json!({"path": ".GIT/config", "content": "x"}))
            .await;
        assert!(case_variant.is_err());
        let edit = EditFile
            .execute(
                &context,
                json!({"path": ".git/config", "old_string": "[core]", "new_string": "x"}),
            )
            .await
            .unwrap();
        assert!(edit.is_error);
        assert_eq!(
            std::fs::read_to_string(context.root.join(".git/config")).unwrap(),
            "[core]\n"
        );
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
            "2001::1",
            "2002:7f00:1::1",
            "::192.168.1.1",
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_confines_writes_to_worktree_and_scratch() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("worktree");
        std::fs::create_dir(&root).unwrap();
        let context = ToolContext {
            root: root.clone(),
            project_root: root.clone(),
            sandbox_root: dir.path().join("sandbox"),
            home: dir.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            secret_roots: Vec::new(),
            network_allowed: true,
        };
        let inside = Shell
            .execute(&context, json!({"command": "echo inside > inside.txt"}))
            .await
            .unwrap();
        assert!(!inside.is_error, "{}", inside.content);
        assert!(root.join("inside.txt").exists());

        let outside = dir.path().join("outside.txt");
        let escaped = Shell
            .execute(
                &context,
                json!({"command": format!("echo bad > {}", outside.display())}),
            )
            .await
            .unwrap();
        assert!(escaped.is_error, "{}", escaped.content);
        assert!(!outside.exists());

        let scratch = Shell
            .execute(
                &context,
                json!({"command": "mktemp >/dev/null; echo tmp-ok"}),
            )
            .await
            .unwrap();
        assert!(!scratch.is_error, "{}", scratch.content);
        assert!(scratch.content.contains("tmp-ok"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_home_read_jail_denies_secrets_but_allows_worktree() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("worktree");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("visible.txt"), "worktree\n").unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("secret.txt"), "secret\n").unwrap();
        let context = ToolContext {
            root: root.clone(),
            project_root: root.clone(),
            sandbox_root: dir.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: true,
            home: home.clone(),
            restrict_home: true,
            read_jail: false,
            git_branch: None,
        };
        let visible = Shell
            .execute(&context, json!({"command": "cat visible.txt"}))
            .await
            .unwrap();
        assert!(!visible.is_error, "{}", visible.content);
        assert!(visible.content.contains("worktree"));

        let secret = Shell
            .execute(&context, json!({"command": "cat $HOME/secret.txt"}))
            .await
            .unwrap();
        assert!(secret.is_error, "{}", secret.content);
        assert!(secret.content.contains("Operation not permitted"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_read_jail_denies_home_siblings_but_allows_worktree() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("worktree");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("visible.txt"), "worktree\n").unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("secret.txt"), "secret\n").unwrap();
        let context = ToolContext {
            root: root.clone(),
            project_root: root.clone(),
            sandbox_root: dir.path().join("sandbox"),
            secret_roots: Vec::new(),
            network_allowed: true,
            home,
            restrict_home: false,
            read_jail: true,
            git_branch: None,
        };
        let visible = Shell
            .execute(&context, json!({"command": "cat visible.txt"}))
            .await
            .unwrap();
        assert!(!visible.is_error, "{}", visible.content);
        assert!(visible.content.contains("worktree"));

        let secret = Shell
            .execute(&context, json!({"command": "cat ../home/secret.txt"}))
            .await
            .unwrap();
        assert!(secret.is_error, "{}", secret.content);
        assert!(secret.content.contains("Operation not permitted"));

        let system = Shell
            .execute(&context, json!({"command": "cat /etc/hosts >/dev/null"}))
            .await
            .unwrap();
        assert!(!system.is_error, "{}", system.content);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_network_policy_can_deny_access() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("worktree");
        std::fs::create_dir(&root).unwrap();
        let context = ToolContext {
            root: root.clone(),
            project_root: root,
            sandbox_root: dir.path().join("sandbox"),
            home: dir.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            secret_roots: Vec::new(),
            network_allowed: false,
        };
        let output = Shell
            .execute(
                &context,
                json!({"command": "curl -sS --max-time 2 https://example.com >/dev/null"}),
            )
            .await
            .unwrap();
        assert!(output.is_error, "{}", output.content);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_timeout_kills_background_processes() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("worktree");
        std::fs::create_dir(&root).unwrap();
        let context = ToolContext {
            root: root.clone(),
            project_root: root.clone(),
            sandbox_root: dir.path().join("sandbox"),
            home: dir.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            secret_roots: Vec::new(),
            network_allowed: true,
        };
        let started = std::time::Instant::now();
        let result = Shell
            .execute(
                &context,
                json!({
                    "command": "(sleep 2; echo survived > marker) & wait",
                    "timeout_secs": 1
                }),
            )
            .await;
        let result = result.unwrap();
        assert!(result.is_error, "{}", result.content);
        assert!(result.content.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::sleep(Duration::from_millis(2200)).await;
        assert!(!root.join("marker").exists());
    }

    #[tokio::test]
    async fn shell_output_is_bounded() {
        let (dir, context) = ctx();
        std::fs::write(dir.path().join("large.txt"), vec![b'x'; 3_000_000]).unwrap();
        let output = Shell
            .execute(&context, json!({"command": "cat large.txt"}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("stdout capped at 1 MiB"));
        assert!(output.content.len() < MAX_SHELL_STREAM_BYTES + 4_000);
        drop(dir);
    }

    #[tokio::test]
    async fn read_file_rejects_oversized_files() {
        let (dir, context) = ctx();
        let path = dir.path().join("large.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_READ_FILE_BYTES + 1).unwrap();
        let output = ReadFile
            .execute(&context, json!({"path": "large.bin"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("read_file is limited"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_allows_git_writes_in_a_linked_worktree() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_std_git(&repo, &["init", "-q"]);
        run_std_git(&repo, &["config", "user.email", "test@example.com"]);
        run_std_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        run_std_git(&repo, &["add", "a.txt"]);
        run_std_git(&repo, &["commit", "-qm", "init"]);
        let worktree = dir.path().join("task");
        run_std_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().unwrap(),
                "-b",
                "task",
            ],
        );
        let context = ToolContext {
            root: worktree.clone(),
            project_root: repo.clone(),
            sandbox_root: dir.path().join("sandbox"),
            home: dir.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: Some("task".to_string()),
            secret_roots: Vec::new(),
            network_allowed: true,
        };
        let output = Shell
            .execute(
                &context,
                json!({
                    "command": "echo two > b.txt && git add b.txt && git -c user.email=test@example.com -c user.name=Test commit -qm task && git log --oneline -1"
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(worktree.join("b.txt").exists());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(status.stdout.is_empty());

        let common = repo.join(".git");
        for target in [common.join("config"), common.join("hooks/pre-commit")] {
            let output = Shell
                .execute(
                    &context,
                    json!({"command": format!("echo hacked >> {}", target.display())}),
                )
                .await
                .unwrap();
            assert!(output.is_error, "{} was writable", target.display());
        }

        let pristine_gitfile = std::fs::read_to_string(worktree.join(".git")).unwrap();
        let output = Shell
            .execute(&context, json!({"command": "echo hacked > .git"}))
            .await
            .unwrap();
        assert!(output.is_error, "{}", output.content);
        assert_eq!(
            std::fs::read_to_string(worktree.join(".git")).unwrap(),
            pristine_gitfile
        );

        let output = Shell
            .execute(
                &context,
                json!({"command": "git update-ref refs/heads/main HEAD"}),
            )
            .await
            .unwrap();
        assert!(output.is_error, "{}", output.content);

        let output = Shell
            .execute(
                &context,
                json!({"command": "echo hacked > \"$(git rev-parse --git-dir)/HEAD\""}),
            )
            .await
            .unwrap();
        assert!(output.is_error, "{}", output.content);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn shell_does_not_trust_a_tampered_git_file() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_std_git(&repo, &["init", "-q"]);
        let worktree = dir.path().join("task");
        std::fs::create_dir(&worktree).unwrap();
        let target = dir.path().join("outside");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", target.display()),
        )
        .unwrap();
        let context = ToolContext {
            root: worktree,
            project_root: repo,
            sandbox_root: dir.path().join("sandbox"),
            home: dir.path().join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            secret_roots: Vec::new(),
            network_allowed: true,
        };
        let output = Shell
            .execute(
                &context,
                json!({"command": format!("echo pwn > {}/file", target.display())}),
            )
            .await
            .unwrap();
        assert!(output.is_error, "{}", output.content);
        assert!(!target.join("file").exists());
    }

    #[cfg(target_os = "macos")]
    fn run_std_git(root: &Path, args: &[&str]) {
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
    }
}

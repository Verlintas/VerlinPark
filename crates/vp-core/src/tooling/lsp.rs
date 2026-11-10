//! Optional Language Server Protocol intelligence.
//!
//! Lexical tools work without extra dependencies. This module adds a real LSP
//! client for projects that already have a language server installed. Servers
//! are spawned under the same shell sandbox as other tools, are cached per
//! worktree/language, and are shut down after an idle period or when the
//! worktree is removed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::time::timeout;

use crate::tools::{
    AgentTool, ToolContext, ToolOutput, safe_existing_path, sandboxed_command, truncate,
};

const LSP_REQUEST_TIMEOUT_SECS: u64 = 30;
const LSP_START_TIMEOUT_SECS: u64 = 60;
const LSP_IDLE_SECS: u64 = 900;
const MAX_LSP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HOVER_CHARS: usize = 12_000;
const MAX_SYMBOLS: usize = 500;

pub struct LspQuery;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerKey {
    root: PathBuf,
    language: String,
}

#[derive(Debug, Clone, Copy)]
struct LspLanguage {
    id: &'static str,
    candidates: &'static [(&'static str, &'static [&'static str])],
}

fn language_for_path(path: &Path) -> Option<LspLanguage> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some(LspLanguage {
            id: "rust",
            candidates: &[("rust-analyzer", &[])],
        }),
        "ts" | "tsx" | "mts" | "cts" => Some(LspLanguage {
            id: "typescript",
            candidates: &[("typescript-language-server", &["--stdio"])],
        }),
        "js" | "jsx" | "mjs" | "cjs" => Some(LspLanguage {
            id: "javascript",
            candidates: &[("typescript-language-server", &["--stdio"])],
        }),
        "py" | "pyi" => Some(LspLanguage {
            id: "python",
            candidates: &[("pyright-langserver", &["--stdio"]), ("pylsp", &[])],
        }),
        "go" => Some(LspLanguage {
            id: "go",
            candidates: &[("gopls", &[])],
        }),
        "swift" => Some(LspLanguage {
            id: "swift",
            candidates: &[("sourcekit-lsp", &[])],
        }),
        "c" | "h" => Some(LspLanguage {
            id: "c",
            candidates: &[("clangd", &[])],
        }),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => Some(LspLanguage {
            id: "cpp",
            candidates: &[("clangd", &[])],
        }),
        "java" => Some(LspLanguage {
            id: "java",
            candidates: &[("jdtls", &[])],
        }),
        "kt" | "kts" => Some(LspLanguage {
            id: "kotlin",
            candidates: &[("kotlin-language-server", &[])],
        }),
        _ => None,
    }
}

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;
type DiagnosticsMap = Arc<Mutex<HashMap<String, Value>>>;

struct LspServer {
    key: ServerKey,
    pid: Option<u32>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: PendingMap,
    next_id: AtomicI64,
    diagnostics: DiagnosticsMap,
    open_docs: Arc<Mutex<HashMap<String, i64>>>,
    alive: Arc<AtomicBool>,
    last_used: Mutex<Instant>,
}

type SharedServer = Arc<LspServer>;

static LSP_SERVERS: OnceLock<Mutex<HashMap<ServerKey, SharedServer>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<ServerKey, SharedServer>> {
    LSP_SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stop all language servers for a worktree. Called when the worktree is
/// removed; idle servers are also reaped on each LSP request.
pub(crate) fn stop_lsp_servers(root: &Path) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let Ok(mut servers) = registry().lock() else {
        return;
    };
    let keys = servers
        .iter()
        .filter(|(key, _)| key.root == root)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(server) = servers.remove(&key) {
            kill_server(&server);
        }
    }
}

fn kill_server(server: &LspServer) {
    server.alive.store(false, Ordering::SeqCst);
    if let Some(pid) = server.pid
        && let Ok(pid) = i32::try_from(pid)
    {
        #[cfg(unix)]
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
    if let Ok(mut pending) = server.pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err("language server stopped".to_string()));
        }
    }
}

fn reap_idle_servers() {
    let Ok(mut servers) = registry().lock() else {
        return;
    };
    let now = Instant::now();
    let stale = servers
        .iter()
        .filter_map(|(key, server)| {
            let idle = server
                .last_used
                .lock()
                .map(|last| now.duration_since(*last) > Duration::from_secs(LSP_IDLE_SECS))
                .unwrap_or(false);
            (!server.alive.load(Ordering::SeqCst) || idle).then_some(key.clone())
        })
        .collect::<Vec<_>>();
    for key in stale {
        if let Some(server) = servers.remove(&key) {
            kill_server(&server);
        }
    }
}

async fn get_server(context: &ToolContext, language: LspLanguage) -> Result<SharedServer> {
    reap_idle_servers();
    let root = context
        .root
        .canonicalize()
        .context("resolving LSP workspace root")?;
    let key = ServerKey {
        root: root.clone(),
        language: language.id.to_string(),
    };
    if let Some(server) = registry()
        .lock()
        .ok()
        .and_then(|servers| servers.get(&key).cloned())
    {
        if server.alive.load(Ordering::SeqCst) {
            if let Ok(mut last) = server.last_used.lock() {
                *last = Instant::now();
            }
            return Ok(server);
        }
        if let Ok(mut servers) = registry().lock() {
            servers.remove(&key);
        }
        kill_server(&server);
    }
    let server = start_server(context, &key, language).await?;
    if let Ok(mut servers) = registry().lock() {
        servers.insert(key, server.clone());
    }
    Ok(server)
}

async fn start_server(
    context: &ToolContext,
    key: &ServerKey,
    language: LspLanguage,
) -> Result<SharedServer> {
    let (program, args) = discover_server(language).ok_or_else(|| {
        anyhow!(
            "no {} language server found on PATH; install one of: {}",
            language.id,
            language
                .candidates
                .iter()
                .map(|(program, _)| *program)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    start_server_with(context, key, &program, args).await
}

async fn start_server_with(
    context: &ToolContext,
    key: &ServerKey,
    program: &str,
    args: &[&str],
) -> Result<SharedServer> {
    let mut command = sandboxed_command(context).await?;
    command
        .arg(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("spawning language server {program}"))?;
    let pid = child.id();
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("language server stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("language server stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("language server stderr is unavailable"))?;
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let stdin = Arc::new(AsyncMutex::new(stdin));
    let diagnostics = Arc::new(Mutex::new(HashMap::new()));
    let open_docs = Arc::new(Mutex::new(HashMap::new()));
    let alive = Arc::new(AtomicBool::new(true));
    let root_uri = file_uri(&key.root)?;
    let root_name = key
        .root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace")
        .to_string();
    let reader_pending = pending.clone();
    let reader_diagnostics = diagnostics.clone();
    let reader_alive = alive.clone();
    let reader_stdin = stdin.clone();
    let reader_root_uri = root_uri.clone();
    tokio::spawn(reader_loop(
        stdout,
        reader_stdin,
        reader_pending,
        reader_diagnostics,
        reader_alive,
        reader_root_uri,
        root_name,
    ));
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line.len() > 4_000 {
                        line.truncate(4_000);
                    }
                }
            }
        }
    });
    let server = Arc::new(LspServer {
        key: key.clone(),
        pid,
        stdin,
        pending,
        next_id: AtomicI64::new(1),
        diagnostics,
        open_docs,
        alive,
        last_used: Mutex::new(Instant::now()),
    });
    let init_params = json!({
        "processId": null,
        "clientInfo": {"name": "VerlinPark", "version": "0.1"},
        "rootUri": root_uri,
        "capabilities": {
            "workspace": {
                "workspaceFolders": true,
                "configuration": true,
                "workspaceEdit": {"documentChanges": true}
            },
            "textDocument": {
                "synchronization": {"dynamicRegistration": false, "didSave": true},
                "definition": {"linkSupport": true},
                "references": {},
                "hover": {"contentFormat": ["markdown", "plaintext"]},
                "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                "publishDiagnostics": {"relatedInformation": true, "versionSupport": true},
                "diagnostic": {"dynamicRegistration": false}
            },
            "window": {"workDoneProgress": true}
        },
        "workspaceFolders": [{"uri": root_uri, "name": key.root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace")}],
        "initializationOptions": {}
    });
    let initialized = server
        .request_with_timeout(
            "initialize",
            init_params,
            Duration::from_secs(LSP_START_TIMEOUT_SECS),
        )
        .await;
    if let Err(error) = initialized {
        kill_server(&server);
        return Err(anyhow!("language server initialize failed: {error}"));
    }
    server
        .notify("initialized", json!({}))
        .await
        .context("sending initialized notification")?;
    Ok(server)
}

fn discover_server(language: LspLanguage) -> Option<(String, &'static [&'static str])> {
    for (program, args) in language.candidates {
        if find_on_path(program).is_some() {
            return Some(((*program).to_string(), *args));
        }
    }
    None
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if candidate
                    .metadata()
                    .ok()
                    .is_some_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
                {
                    return Some(candidate);
                }
            }
            #[cfg(not(unix))]
            {
                return Some(candidate);
            }
        }
    }
    None
}

impl LspServer {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(
            method,
            params,
            Duration::from_secs(LSP_REQUEST_TIMEOUT_SECS),
        )
        .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value> {
        if !self.alive.load(Ordering::SeqCst) {
            bail!("language server is not running");
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, sender);
        }
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = write_message(&self.stdin, &message).await {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&id);
            }
            return Err(error);
        }
        match timeout(wait, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(anyhow!(error)),
            Ok(Err(_)) => Err(anyhow!("language server response channel closed")),
            Err(_) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                Err(anyhow!(
                    "language server did not respond to {method} within {}s",
                    wait.as_secs()
                ))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        write_message(&self.stdin, &message).await
    }

    async fn open_or_update(&self, path: &Path) -> Result<String> {
        let uri = file_uri(path)?;
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {} for LSP", path.display()))?;
        let (was_open, version) = {
            let mut docs = self
                .open_docs
                .lock()
                .map_err(|_| anyhow!("LSP document map lock poisoned"))?;
            let was_open = docs.contains_key(&uri);
            let version = docs.get(&uri).copied().unwrap_or(0) + 1;
            docs.insert(uri.clone(), version);
            (was_open, version)
        };
        if was_open {
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": text}],
                }),
            )
            .await?;
        } else {
            self.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": self.key.language,
                        "version": version,
                        "text": text,
                    }
                }),
            )
            .await?;
        }
        Ok(uri)
    }
}

async fn reader_loop(
    stdout: tokio::process::ChildStdout,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: PendingMap,
    diagnostics: DiagnosticsMap,
    alive: Arc<AtomicBool>,
    root_uri: String,
    root_name: String,
) {
    let mut reader = BufReader::new(stdout);
    while let Ok(message) = read_message(&mut reader).await {
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            if message.get("result").is_some() || message.get("error").is_some() {
                if let Ok(mut pending) = pending.lock()
                    && let Some(sender) = pending.remove(&id)
                {
                    let result = if let Some(error) = message.get("error") {
                        Err(error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("language server error")
                            .to_string())
                    } else {
                        Ok(message.get("result").cloned().unwrap_or(Value::Null))
                    };
                    let _ = sender.send(result);
                }
            } else if let Some(method) = message.get("method").and_then(Value::as_str) {
                let response = match method {
                    "workspace/configuration" => {
                        let count = message
                            .get("params")
                            .and_then(|params| params.get("items"))
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        json!(vec![Value::Null; count])
                    }
                    "workspace/workspaceFolders" => {
                        json!([{"uri": root_uri, "name": root_name}])
                    }
                    _ => Value::Null,
                };
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": response,
                });
                let _ = write_message(&stdin, &reply).await;
            }
        } else if let Some(method) = message.get("method").and_then(Value::as_str)
            && method == "textDocument/publishDiagnostics"
            && let Some(params) = message.get("params")
            && let Some(uri) = params.get("uri").and_then(Value::as_str)
            && let Ok(mut diagnostics) = diagnostics.lock()
        {
            diagnostics.insert(uri.to_string(), params.clone());
        }
    }
    alive.store(false, Ordering::SeqCst);
    if let Ok(mut pending) = pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err("language server exited".to_string()));
        }
    }
}

async fn write_message(stdin: &Arc<AsyncMutex<ChildStdin>>, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    if body.len() > MAX_LSP_MESSAGE_BYTES {
        bail!("LSP message exceeds 16 MiB");
    }
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut stdin = stdin.lock().await;
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_message<R: tokio::io::AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Result<Value> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            bail!("language server closed stdout");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            let value = value.trim().parse::<usize>()?;
            if value > MAX_LSP_MESSAGE_BYTES {
                bail!("language server sent an oversized message");
            }
            content_length = Some(value);
        }
    }
    let length = content_length.ok_or_else(|| anyhow!("LSP message has no Content-Length"))?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

fn file_uri(path: &Path) -> Result<String> {
    reqwest::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| anyhow!("path cannot be converted to a file URI: {}", path.display()))
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    reqwest::Url::parse(uri).ok()?.to_file_path().ok()
}

fn position_for(path: &Path, line: u64, column: u64) -> Result<(u64, u64)> {
    if line == 0 || column == 0 {
        bail!("line and column are 1-based and must be positive");
    }
    let text = std::fs::read_to_string(path)?;
    let line_text = text
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .ok_or_else(|| anyhow!("line {line} is beyond the end of {}", path.display()))?;
    let character = line_text
        .chars()
        .take(column.saturating_sub(1) as usize)
        .map(char::len_utf16)
        .sum::<usize>() as u64;
    Ok((line - 1, character))
}

fn range_start(range: &Value) -> Option<(u64, u64)> {
    Some((
        range.get("start")?.get("line")?.as_u64()? + 1,
        range.get("start")?.get("character")?.as_u64()? + 1,
    ))
}

fn range_end(range: &Value) -> Option<(u64, u64)> {
    Some((
        range.get("end")?.get("line")?.as_u64()? + 1,
        range.get("end")?.get("character")?.as_u64()? + 1,
    ))
}

fn location_json(root: &Path, location: &Value) -> Option<Value> {
    let uri = location
        .get("uri")
        .or_else(|| location.get("targetUri"))
        .and_then(Value::as_str)?;
    let path = uri_to_path(uri)?;
    let range = location
        .get("range")
        .or_else(|| location.get("targetSelectionRange"))
        .or_else(|| location.get("targetRange"))?;
    let (line, column) = range_start(range)?;
    let (end_line, end_column) = range_end(range)?;
    let context = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| {
            text.lines()
                .nth(line.saturating_sub(1) as usize)
                .map(str::to_string)
        })
        .unwrap_or_default();
    Some(json!({
        "path": path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string(),
        "line": line,
        "column": column,
        "end_line": end_line,
        "end_column": end_column,
        "context": context.chars().take(300).collect::<String>(),
    }))
}

fn locations(root: &Path, value: &Value, limit: usize) -> Vec<Value> {
    let values = if let Some(array) = value.as_array() {
        array.iter().collect::<Vec<_>>()
    } else if value.is_object() {
        vec![value]
    } else {
        Vec::new()
    };
    values
        .into_iter()
        .filter_map(|location| location_json(root, location))
        .take(limit)
        .collect()
}

fn hover_text(contents: &Value) -> String {
    let raw = match contents {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                Value::Object(object) => object
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    };
    raw.chars().take(MAX_HOVER_CHARS).collect()
}

#[async_trait]
impl AgentTool for LspQuery {
    fn name(&self) -> &'static str {
        "lsp_query"
    }

    fn description(&self) -> &'static str {
        "Query an installed language server for definitions, references, hover, document/workspace symbols, or diagnostics. Falls back to an explicit error when no server is installed."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["definition", "references", "hover", "symbols", "workspace_symbols", "diagnostics"]},
                "path": {"type": "string", "description": "File path relative to the worktree root"},
                "line": {"type": "integer", "minimum": 1, "description": "1-based line"},
                "column": {"type": "integer", "minimum": 1, "description": "1-based column"},
                "query": {"type": "string", "description": "Workspace symbol query"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "description": "Default 100"},
                "include_declaration": {"type": "boolean", "description": "References include the declaration; default true"}
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("action is required"))?;
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, MAX_SYMBOLS as u64) as usize;
        let path = input.get("path").and_then(Value::as_str);
        if action != "workspace_symbols" && path.is_none() {
            return Ok(ToolOutput::error_structured(
                "path is required for this LSP action",
                json!({"code": "INVALID_ARGUMENT", "action": action}),
            ));
        }
        let file = if let Some(path) = path {
            let file = safe_existing_path(&context.root, path)?;
            if !file.is_file() {
                return Ok(ToolOutput::error_structured(
                    "LSP path must be a file",
                    json!({"code": "INVALID_ARGUMENT", "path": path}),
                ));
            }
            Some(file)
        } else {
            None
        };
        let language = if let Some(file) = &file {
            language_for_path(file)
        } else {
            // Workspace symbols need a language. Use the first source file in
            // the worktree to choose a server.
            first_source_file(&context.root).and_then(|path| language_for_path(&path))
        };
        let Some(language) = language else {
            return Ok(ToolOutput::error_structured(
                "no LSP language mapping for this path",
                json!({"code": "LSP_UNAVAILABLE", "path": path}),
            ));
        };
        let server = match get_server(context, language).await {
            Ok(server) => server,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    error.to_string(),
                    json!({"code": "LSP_UNAVAILABLE", "language": language.id}),
                ));
            }
        };
        let result: Result<(Value, String)> = match action {
            "definition" | "references" | "hover" => {
                let file = file.as_deref().expect("checked above");
                let line = input
                    .get("line")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("line is required"))?;
                let column = input
                    .get("column")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("column is required"))?;
                let (line, character) = position_for(file, line, column)?;
                let uri = server.open_or_update(file).await?;
                let position = json!({"line": line, "character": character});
                if action == "definition" {
                    let value = server
                        .request(
                            "textDocument/definition",
                            json!({"textDocument": {"uri": uri}, "position": position}),
                        )
                        .await?;
                    let locations = locations(&context.root, &value, limit);
                    let text = render_lsp_result(&json!({"locations": locations}));
                    Ok((json!({"action": action, "locations": locations}), text))
                } else if action == "references" {
                    let include_declaration = input
                        .get("include_declaration")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let value = server
                        .request(
                            "textDocument/references",
                            json!({
                                "textDocument": {"uri": uri},
                                "position": position,
                                "context": {"includeDeclaration": include_declaration},
                            }),
                        )
                        .await?;
                    let locations = locations(&context.root, &value, limit);
                    let text = render_lsp_result(&json!({"locations": locations}));
                    Ok((json!({"action": action, "locations": locations}), text))
                } else {
                    let value = server
                        .request(
                            "textDocument/hover",
                            json!({"textDocument": {"uri": uri}, "position": position}),
                        )
                        .await?;
                    let text = value.get("contents").map(hover_text).unwrap_or_default();
                    let range = value.get("range").and_then(|range| {
                        let (line, column) = range_start(range)?;
                        let (end_line, end_column) = range_end(range)?;
                        Some(json!({
                            "line": line,
                            "column": column,
                            "end_line": end_line,
                            "end_column": end_column,
                        }))
                    });
                    Ok((
                        json!({"action": action, "hover": text, "range": range}),
                        text.clone(),
                    ))
                }
            }
            "symbols" => {
                let file = file.as_deref().expect("checked above");
                let uri = server.open_or_update(file).await?;
                let value = server
                    .request(
                        "textDocument/documentSymbol",
                        json!({"textDocument": {"uri": uri}}),
                    )
                    .await?;
                let symbols = document_symbols(&value, &context.root, file, limit);
                Ok((json!({"action": action, "symbols": symbols}), String::new()))
            }
            "workspace_symbols" => {
                let query = input.get("query").and_then(Value::as_str).unwrap_or("");
                let value = server
                    .request("workspace/symbol", json!({"query": query}))
                    .await?;
                let symbols = workspace_symbols(&value, &context.root, limit);
                Ok((
                    json!({"action": action, "query": query, "symbols": symbols}),
                    String::new(),
                ))
            }
            "diagnostics" => {
                let file = file.as_deref().expect("checked above");
                let uri = server.open_or_update(file).await?;
                let pull = server
                    .request(
                        "textDocument/diagnostic",
                        json!({"textDocument": {"uri": uri}, "identifier": null, "previousResultId": null}),
                    )
                    .await;
                let diagnostics = match pull {
                    Ok(value) => diagnostics_from_pull(&value, &context.root, &uri, limit),
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        server
                            .diagnostics
                            .lock()
                            .ok()
                            .and_then(|diagnostics| diagnostics.get(&uri).cloned())
                            .map(|value| diagnostics_from_publish(&value, &context.root, limit))
                            .unwrap_or_default()
                    }
                };
                Ok((
                    json!({"action": action, "diagnostics": diagnostics}),
                    String::new(),
                ))
            }
            other => {
                return Ok(ToolOutput::error_structured(
                    format!("unsupported LSP action: {other}"),
                    json!({"code": "INVALID_ARGUMENT", "action": other}),
                ));
            }
        };
        let (structured, rendered) = match result {
            Ok(value) => value,
            Err(error) => {
                return Ok(ToolOutput::error_structured(
                    error.to_string(),
                    json!({"code": "LSP_ERROR", "action": action}),
                ));
            }
        };
        let text = if !rendered.is_empty() {
            rendered
        } else {
            render_lsp_result(&structured)
        };
        let mut structured = structured;
        structured["confidence"] = json!("lsp");
        Ok(ToolOutput::ok_structured(
            truncate(if text.is_empty() {
                "language server returned no result".to_string()
            } else {
                text
            }),
            structured,
        ))
    }
}

fn render_lsp_result(value: &Value) -> String {
    if let Some(locations) = value.get("locations").and_then(Value::as_array) {
        return locations
            .iter()
            .map(|location| {
                format!(
                    "{}:{}:{} {}",
                    location["path"].as_str().unwrap_or("?"),
                    location["line"].as_u64().unwrap_or(0),
                    location["column"].as_u64().unwrap_or(0),
                    location["context"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Some(symbols) = value.get("symbols").and_then(Value::as_array) {
        return symbols
            .iter()
            .map(|symbol| {
                format!(
                    "{}:{} {} {}",
                    symbol["path"].as_str().unwrap_or("?"),
                    symbol["line"].as_u64().unwrap_or(0),
                    symbol["kind"].as_str().unwrap_or("symbol"),
                    symbol["name"].as_str().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Some(diagnostics) = value.get("diagnostics").and_then(Value::as_array) {
        return diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}:{} {}: {}",
                    diagnostic["path"].as_str().unwrap_or("?"),
                    diagnostic["line"].as_u64().unwrap_or(0),
                    diagnostic["column"].as_u64().unwrap_or(0),
                    diagnostic["severity"].as_str().unwrap_or("error"),
                    diagnostic["message"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    value
        .get("hover")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn first_source_file(root: &Path) -> Option<PathBuf> {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .max_depth(Some(8));
    builder.build().flatten().find_map(|entry| {
        let path = entry.path();
        (entry.file_type().is_some_and(|kind| kind.is_file()) && language_for_path(path).is_some())
            .then(|| path.to_path_buf())
    })
}

fn document_symbols(value: &Value, root: &Path, file: &Path, limit: usize) -> Vec<Value> {
    let mut output = Vec::new();
    if let Some(array) = value.as_array() {
        for symbol in array {
            collect_document_symbol(symbol, None, root, file, &mut output, limit);
            if output.len() >= limit {
                break;
            }
        }
    }
    output
}

fn collect_document_symbol(
    symbol: &Value,
    container: Option<&str>,
    root: &Path,
    file: &Path,
    output: &mut Vec<Value>,
    limit: usize,
) {
    if output.len() >= limit {
        return;
    }
    let name = symbol
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let range = symbol
        .get("selectionRange")
        .or_else(|| {
            symbol
                .get("location")
                .and_then(|location| location.get("range"))
        })
        .or_else(|| symbol.get("range"));
    if let Some(range) = range {
        let (line, column) = range_start(range).unwrap_or((0, 0));
        let (end_line, end_column) = range_end(range).unwrap_or((0, 0));
        output.push(json!({
            "name": name,
            "kind": symbol_kind(symbol.get("kind").and_then(Value::as_u64).unwrap_or(0)),
            "line": line,
            "column": column,
            "end_line": end_line,
            "end_column": end_column,
            "container": container,
            "path": symbol
                .get("location")
                .and_then(|location| location.get("uri"))
                .and_then(Value::as_str)
                .and_then(uri_to_path)
                .map(|path| path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string())
                .unwrap_or_else(|| file.strip_prefix(root).unwrap_or(file).to_string_lossy().to_string()),
        }));
    }
    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            collect_document_symbol(child, Some(&name), root, file, output, limit);
            if output.len() >= limit {
                break;
            }
        }
    }
}

fn workspace_symbols(value: &Value, root: &Path, limit: usize) -> Vec<Value> {
    value
        .as_array()
        .map(|array| {
            array
                .iter()
                .filter_map(|symbol| {
                    let location = symbol.get("location")?;
                    let location = location_json(root, location)?;
                    Some(json!({
                        "name": symbol.get("name").and_then(Value::as_str).unwrap_or("?"),
                        "kind": symbol_kind(symbol.get("kind").and_then(Value::as_u64).unwrap_or(0)),
                        "container": symbol.get("containerName").and_then(Value::as_str),
                        "path": location["path"],
                        "line": location["line"],
                        "column": location["column"],
                    }))
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

fn symbol_kind(kind: u64) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum_member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type_parameter",
        _ => "symbol",
    }
}

fn diagnostics_from_pull(value: &Value, root: &Path, uri: &str, limit: usize) -> Vec<Value> {
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| diagnostics_from_items(items, root, Some(uri), limit))
        .unwrap_or_default()
}

fn diagnostics_from_publish(value: &Value, root: &Path, limit: usize) -> Vec<Value> {
    let uri = value.get("uri").and_then(Value::as_str);
    value
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|items| diagnostics_from_items(items, root, uri, limit))
        .unwrap_or_default()
}

fn diagnostics_from_items(
    items: &[Value],
    root: &Path,
    fallback_uri: Option<&str>,
    limit: usize,
) -> Vec<Value> {
    items
        .iter()
        .filter_map(|diagnostic| {
            let range = diagnostic.get("range")?;
            let (line, column) = range_start(range).unwrap_or((0, 0));
            let (end_line, end_column) = range_end(range).unwrap_or((0, 0));
            let path = diagnostic
                .get("uri")
                .and_then(Value::as_str)
                .or(fallback_uri)
                .and_then(uri_to_path);
            Some(json!({
                "path": path
                    .as_ref()
                    .map(|path| path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string()),
                "line": line,
                "column": column,
                "end_line": end_line,
                "end_column": end_column,
                "severity": diagnostic_severity(diagnostic.get("severity").and_then(Value::as_u64)),
                "code": diagnostic.get("code"),
                "source": diagnostic.get("source"),
                "message": diagnostic.get("message").and_then(Value::as_str).unwrap_or(""),
            }))
        })
        .take(limit)
        .collect()
}

fn diagnostic_severity(severity: Option<u64>) -> &'static str {
    match severity {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "information",
        Some(4) => "hint",
        _ => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn recognizes_languages_and_commands() {
        assert_eq!(
            language_for_path(Path::new("src/lib.rs")).map(|language| language.id),
            Some("rust")
        );
        assert_eq!(
            language_for_path(Path::new("src/app.tsx")).map(|language| language.id),
            Some("typescript")
        );
        assert!(find_on_path("definitely-not-a-real-language-server").is_none());
    }

    #[test]
    fn converts_positions_to_utf16_units() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "a😀b\n").unwrap();
        let (line, character) = position_for(&path, 1, 3).unwrap();
        assert_eq!(line, 0);
        assert_eq!(character, 3);
    }

    #[tokio::test]
    async fn reads_framed_messages() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let frame = format!(
            "Content-Length: {}\r\nX-Test: yes\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        let bytes = frame.into_bytes();
        let mut reader = BufReader::new(bytes.as_slice());
        let value = read_message(&mut reader).await.unwrap();
        assert_eq!(value["id"], 1);
        assert_eq!(value["result"], Value::Null);
    }

    #[test]
    fn maps_symbols_and_diagnostics() {
        let value = json!([{
            "name": "Widget",
            "kind": 23,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 1}},
            "selectionRange": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 12}},
            "children": [{
                "name": "new",
                "kind": 6,
                "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 10}},
                "selectionRange": {"start": {"line": 1, "character": 6}, "end": {"line": 1, "character": 9}}
            }]
        }]);
        let symbols = document_symbols(&value, Path::new("."), Path::new("a.rs"), 10);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["kind"], "struct");
        assert_eq!(symbols[1]["container"], "Widget");

        let diagnostic = json!({
            "range": {"start": {"line": 3, "character": 4}, "end": {"line": 3, "character": 8}},
            "severity": 1,
            "message": "cannot find value",
            "source": "rust-analyzer"
        });
        let diagnostics = diagnostics_from_items(&[diagnostic], Path::new("."), None, 10);
        assert_eq!(diagnostics[0]["line"], 4);
        assert_eq!(diagnostics[0]["severity"], "error");
    }
    #[tokio::test]
    async fn drives_a_fake_language_server_end_to_end() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn hello() {}\n").unwrap();
        std::fs::write(
            root.join("fake_lsp.py"),
            r#"#!/usr/bin/env python3
import json, sys
def read_message():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: return None
        line = line.decode().strip()
        if not line: break
        if line.lower().startswith("content-length:"):
            length = int(line.split(":",1)[1].strip())
    if length is None: return None
    return json.loads(sys.stdin.buffer.read(length))
def send(value):
    body = json.dumps(value).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode()+body)
    sys.stdout.buffer.flush()
uri = None
while True:
    message = read_message()
    if message is None: break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"definitionProvider":True,"referencesProvider":True,"documentSymbolProvider":True,"hoverProvider":True}}})
    elif method == "textDocument/didOpen":
        uri = message["params"]["textDocument"]["uri"]
    elif method == "textDocument/definition":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"uri":uri,"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":8}}}})
    elif method == "textDocument/references":
        send({"jsonrpc":"2.0","id":message["id"],"result":[{"uri":uri,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}}}]})
    elif method == "textDocument/documentSymbol":
        send({"jsonrpc":"2.0","id":message["id"],"result":[{"name":"hello","kind":12,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":14}},"selectionRange":{"start":{"line":0,"character":3},"end":{"line":0,"character":8}}}]})
    elif method == "textDocument/hover":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"contents":{"kind":"markdown","value":"**fn hello()**"}}})
    elif method == "textDocument/diagnostic":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"kind":"full","items":[]}})
    elif method == "shutdown":
        send({"jsonrpc":"2.0","id":message["id"],"result":None})
    elif method == "exit":
        break
"#,
        )
        .unwrap();
        let context = ToolContext {
            root: root.clone(),
            project_root: root.clone(),
            sandbox_root: root.join(".sandbox"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
        };
        let key = ServerKey {
            root: root.clone(),
            language: "rust".into(),
        };
        let server = start_server_with(&context, &key, "python3", &["fake_lsp.py"])
            .await
            .unwrap();
        let file = root.join("src/lib.rs");
        let uri = server.open_or_update(&file).await.unwrap();
        let definition = server
            .request(
                "textDocument/definition",
                json!({"textDocument":{"uri":uri},"position":{"line":0,"character":4}}),
            )
            .await
            .unwrap();
        assert_eq!(definition["uri"], uri);
        let symbols = server
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument":{"uri":uri}}),
            )
            .await
            .unwrap();
        assert_eq!(symbols[0]["name"], "hello");
        let hover = server
            .request(
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":{"line":0,"character":4}}),
            )
            .await
            .unwrap();
        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("hello")
        );
        kill_server(&server);
    }
}

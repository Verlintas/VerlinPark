//! Optional Language Server Protocol intelligence.
//!
//! Lexical tools work without extra dependencies. This module adds a real LSP
//! client for projects that already have a language server installed. Servers
//! are spawned under the same shell sandbox as other tools, are cached per
//! worktree/language, and are shut down after an idle period or when the
//! worktree is removed.
//!
//! The client is deliberately not an IDE runtime: it exposes bounded symbol,
//! definition, reference, hover, and diagnostic queries. It never installs a
//! server and never claims compiler accuracy when a server is unavailable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
use tokio::time::timeout;

use crate::tooling::code_intel::{
    Backend, BackendAttempt, CodeDiagnostic, CodeLocation, CodeRange, CodeSymbol,
    LSP_CONFIDENCE_DEFINITION, LSP_CONFIDENCE_DIAGNOSTIC, LSP_CONFIDENCE_REFERENCE,
    LSP_CONFIDENCE_SYMBOL, Status, backend_sentence, kind_group, prepare_diagnostics,
    prepare_locations, prepare_symbols, render_diagnostics, render_locations, render_symbols,
    score_name_match,
};
use crate::tools::{
    AgentTool, ToolContext, ToolOutput, safe_existing_path, sandboxed_command, truncate,
};

const LSP_REQUEST_TIMEOUT_SECS: u64 = 30;
const LSP_START_TIMEOUT_SECS: u64 = 60;
const LSP_DIAGNOSTIC_REQUEST_SECS: u64 = 12;
const LSP_DIAGNOSTIC_WAIT_MS: u64 = 4_000;
const LSP_DIAGNOSTIC_TOTAL_SECS: u64 = 20;
const LSP_IDLE_SECS: u64 = 900;
const MAX_LSP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_LSP_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HOVER_CHARS: usize = 12_000;
const MAX_SYMBOLS: usize = 500;
const MAX_LOCATIONS: usize = 500;
const MAX_DOCUMENT_SYMBOL_FILES: usize = 24;
const MAX_LANGUAGES_PER_QUERY: usize = 3;
pub(crate) const MAX_DIAGNOSTIC_FILES: usize = 24;
const MAX_CACHED_DIAGNOSTIC_URIS: usize = 256;
const MAX_CACHED_DIAGNOSTICS_PER_URI: usize = 500;
const MAX_STATUS_LANGUAGES: usize = 16;
const RESTART_ATTEMPTS: usize = 1;

pub struct LspQuery;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LspLanguage {
    pub id: &'static str,
    pub display: &'static str,
    extensions: &'static [&'static str],
    candidates: &'static [(&'static str, &'static [&'static str])],
    pub install_hint: &'static str,
}

static LANGUAGES: &[LspLanguage] = &[
    LspLanguage {
        id: "rust",
        display: "Rust",
        extensions: &["rs"],
        candidates: &[("rust-analyzer", &[])],
        install_hint: "Install rust-analyzer (usually `rustup component add rust-analyzer`)",
    },
    LspLanguage {
        id: "typescript",
        display: "TypeScript",
        extensions: &["ts", "tsx", "mts", "cts"],
        candidates: &[("typescript-language-server", &["--stdio"])],
        install_hint: "Install typescript-language-server (`npm install -g typescript-language-server typescript`)",
    },
    LspLanguage {
        id: "javascript",
        display: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        candidates: &[("typescript-language-server", &["--stdio"])],
        install_hint: "Install typescript-language-server (`npm install -g typescript-language-server typescript`)",
    },
    LspLanguage {
        id: "python",
        display: "Python",
        extensions: &["py", "pyi"],
        candidates: &[("pyright-langserver", &["--stdio"]), ("pylsp", &[])],
        install_hint: "Install pyright (`npm install -g pyright`) or python-lsp-server (`pip install python-lsp-server`)",
    },
    LspLanguage {
        id: "go",
        display: "Go",
        extensions: &["go"],
        candidates: &[("gopls", &[])],
        install_hint: "Install gopls (`go install golang.org/x/tools/gopls@latest`)",
    },
    LspLanguage {
        id: "swift",
        display: "Swift",
        extensions: &["swift"],
        candidates: &[("sourcekit-lsp", &[])],
        install_hint: "Install sourcekit-lsp with the Swift toolchain",
    },
    LspLanguage {
        id: "c",
        display: "C",
        extensions: &["c", "h"],
        candidates: &[("clangd", &[])],
        install_hint: "Install clangd (LLVM or `brew install llvm`)",
    },
    LspLanguage {
        id: "cpp",
        display: "C++",
        extensions: &["cc", "cpp", "cxx", "hpp", "hh"],
        candidates: &[("clangd", &[])],
        install_hint: "Install clangd (LLVM or `brew install llvm`)",
    },
    LspLanguage {
        id: "java",
        display: "Java",
        extensions: &["java"],
        candidates: &[("jdtls", &[])],
        install_hint: "Install Eclipse JDT Language Server (`jdtls`)",
    },
    LspLanguage {
        id: "kotlin",
        display: "Kotlin",
        extensions: &["kt", "kts"],
        candidates: &[("kotlin-language-server", &[])],
        install_hint: "Install kotlin-language-server",
    },
];

pub(crate) fn all_languages() -> &'static [LspLanguage] {
    LANGUAGES
}

pub(crate) fn language_for_path(path: &Path) -> Option<LspLanguage> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    LANGUAGES
        .iter()
        .copied()
        .find(|language| language.extensions.contains(&extension.as_str()))
}

pub(crate) fn language_by_name(name: &str) -> Option<LspLanguage> {
    let name = name.trim();
    LANGUAGES.iter().copied().find(|language| {
        language.id.eq_ignore_ascii_case(name) || language.display.eq_ignore_ascii_case(name)
    })
}

/// Choose the languages present in a file set, ordered by file count and then
/// language id. The cap prevents a polyglot repository from spawning every
/// server at once.
pub(crate) fn languages_for_files(files: &[PathBuf], limit: usize) -> Vec<LspLanguage> {
    let mut counts: HashMap<&'static str, (LspLanguage, usize)> = HashMap::new();
    for file in files {
        if let Some(language) = language_for_path(file) {
            counts
                .entry(language.id)
                .and_modify(|(_, count)| *count += 1)
                .or_insert((language, 1));
        }
    }
    let mut languages = counts.into_values().collect::<Vec<_>>();
    languages.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.id.cmp(right.0.id)));
    languages
        .into_iter()
        .take(limit.clamp(1, MAX_LANGUAGES_PER_QUERY))
        .map(|(language, _)| language)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerKey {
    root: PathBuf,
    language: String,
}

#[derive(Debug, Clone)]
struct ConnectFailure {
    failures: u32,
    retry_at: Instant,
    message: String,
}

static LSP_CONNECT_FAILURES: OnceLock<Mutex<HashMap<ServerKey, ConnectFailure>>> = OnceLock::new();

fn connect_failures() -> &'static Mutex<HashMap<ServerKey, ConnectFailure>> {
    LSP_CONNECT_FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn failure_backoff(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(5);
    Duration::from_secs(5u64.saturating_mul(1u64 << exponent).min(60))
}

fn clear_connect_failure(key: &ServerKey) {
    if let Ok(mut failures) = connect_failures().lock() {
        failures.remove(key);
    }
}

fn cooldown_for(key: &ServerKey) -> Option<(u64, String)> {
    let now = Instant::now();
    let mut failures = connect_failures().lock().ok()?;
    if let Some(failure) = failures.get(key) {
        if failure.retry_at > now {
            return Some((
                failure.retry_at.duration_since(now).as_secs().max(1),
                failure.message.clone(),
            ));
        }
        failures.remove(key);
    }
    None
}

fn record_connect_failure(key: &ServerKey, message: &str) {
    if let Ok(mut failures) = connect_failures().lock() {
        let failures_count = failures
            .get(key)
            .map(|failure| failure.failures)
            .unwrap_or(0)
            + 1;
        failures.insert(
            key.clone(),
            ConnectFailure {
                failures: failures_count,
                retry_at: Instant::now() + failure_backoff(failures_count),
                message: message.chars().take(500).collect(),
            },
        );
    }
}

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspError>>>>>;
type DiagnosticsMap = Arc<Mutex<HashMap<String, PublishedDiagnostic>>>;
type PullCache = Arc<Mutex<HashMap<String, PulledDiagnostic>>>;

#[derive(Debug, Clone)]
struct PublishedDiagnostic {
    params: Value,
    version: Option<i64>,
    received: Instant,
}

#[derive(Debug, Clone)]
struct PulledDiagnostic {
    value: Value,
    received: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LspServerInfo {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PositionEncoding {
    #[default]
    Utf16,
    Utf8,
    Utf32,
}

impl PositionEncoding {
    fn from_capabilities(capabilities: &Value) -> Self {
        match capabilities
            .get("positionEncoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-16")
            .to_ascii_lowercase()
            .as_str()
        {
            "utf-8" => Self::Utf8,
            "utf-32" => Self::Utf32,
            _ => Self::Utf16,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Utf16 => "utf-16",
            Self::Utf8 => "utf-8",
            Self::Utf32 => "utf-32",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LspCapabilities {
    pub definition: bool,
    pub references: bool,
    pub hover: bool,
    pub document_symbols: bool,
    pub workspace_symbols: bool,
    pub pull_diagnostics: bool,
    /// The protocol has no server capability for push diagnostics. Opening a
    /// document may still produce `textDocument/publishDiagnostics`; this flag
    /// records that the client accepts them.
    pub publish_diagnostics: bool,
    pub position_encoding: PositionEncoding,
}

impl LspCapabilities {
    fn from_initialize(result: &Value) -> Self {
        let capabilities = result.get("capabilities").unwrap_or(&Value::Null);
        Self {
            definition: provider_enabled(capabilities.get("definitionProvider")),
            references: provider_enabled(capabilities.get("referencesProvider")),
            hover: provider_enabled(capabilities.get("hoverProvider")),
            document_symbols: provider_enabled(capabilities.get("documentSymbolProvider")),
            workspace_symbols: provider_enabled(capabilities.get("workspaceSymbolProvider")),
            pull_diagnostics: provider_enabled(capabilities.get("diagnosticProvider")),
            publish_diagnostics: true,
            position_encoding: PositionEncoding::from_capabilities(capabilities),
        }
    }

    pub(crate) fn supports(&self, method: &str) -> bool {
        match method {
            "textDocument/definition" => self.definition,
            "textDocument/references" => self.references,
            "textDocument/hover" => self.hover,
            "textDocument/documentSymbol" => self.document_symbols,
            "workspace/symbol" => self.workspace_symbols,
            "textDocument/diagnostic" => self.pull_diagnostics,
            "textDocument/publishDiagnostics" => self.publish_diagnostics,
            _ => false,
        }
    }

    pub(crate) fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.definition {
            names.push("definition");
        }
        if self.references {
            names.push("references");
        }
        if self.hover {
            names.push("hover");
        }
        if self.document_symbols {
            names.push("document_symbols");
        }
        if self.workspace_symbols {
            names.push("workspace_symbols");
        }
        if self.pull_diagnostics {
            names.push("pull_diagnostics");
        }
        if self.publish_diagnostics {
            names.push("publish_diagnostics");
        }
        names
    }

    pub(crate) fn summary(&self) -> Value {
        json!({
            "definition": self.definition,
            "references": self.references,
            "hover": self.hover,
            "document_symbols": self.document_symbols,
            "workspace_symbols": self.workspace_symbols,
            "pull_diagnostics": self.pull_diagnostics,
            "publish_diagnostics": self.publish_diagnostics,
            "position_encoding": self.position_encoding.as_str(),
        })
    }
}

fn provider_enabled(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)) | Some(Value::Object(_)))
}

struct LspServer {
    key: ServerKey,
    language: LspLanguage,
    program: String,
    args: Vec<String>,
    pid: Option<u32>,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: PendingMap,
    next_id: AtomicI64,
    diagnostics: DiagnosticsMap,
    diagnostics_notify: Arc<Notify>,
    pull_cache: PullCache,
    open_docs: Arc<Mutex<HashMap<String, i64>>>,
    alive: Arc<AtomicBool>,
    capabilities: LspCapabilities,
    server_info: Option<LspServerInfo>,
    last_used: Mutex<Instant>,
}

type SharedServer = Arc<LspServer>;

static LSP_SERVERS: OnceLock<Mutex<HashMap<ServerKey, SharedServer>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<ServerKey, SharedServer>> {
    LSP_SERVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub(crate) struct LspConnection {
    server: SharedServer,
}

impl LspConnection {
    #[cfg(test)]
    pub(crate) fn capabilities(&self) -> LspCapabilities {
        self.server.capabilities.clone()
    }

    pub(crate) fn info(&self) -> LspConnectionInfo {
        LspConnectionInfo {
            language: self.server.language.id.to_string(),
            display_language: self.server.language.display.to_string(),
            server: self
                .server
                .server_info
                .as_ref()
                .map(|info| info.name.clone()),
            server_version: self
                .server
                .server_info
                .as_ref()
                .and_then(|info| info.version.clone()),
            capabilities: self.server.capabilities.clone(),
        }
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.server.key.root.clone()
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn is_alive(&self) -> bool {
        self.server.alive.load(Ordering::SeqCst)
    }

    pub(crate) async fn open_document(&self, path: &Path) -> Result<OpenDocument, LspError> {
        self.server.open_or_update_versioned(path).await
    }

    #[cfg(test)]
    pub(crate) async fn open_or_update(&self, path: &Path) -> Result<String, LspError> {
        Ok(self.server.open_or_update_versioned(path).await?.uri)
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        self.server.request(method, params).await
    }

    pub(crate) async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, LspError> {
        self.server.request_with_timeout(method, params, wait).await
    }

    #[cfg(test)]
    pub(crate) async fn hover(
        &self,
        path: &Path,
        line: u64,
        column: u64,
    ) -> Result<Value, LspError> {
        let (line, character) = position_for(
            path,
            line,
            column,
            self.server.capabilities.position_encoding,
        )
        .map_err(|error| LspError::Message(error.to_string()))?;
        let uri = self.open_document(path).await?.uri;
        self.request(
            "textDocument/hover",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        )
        .await
    }

    fn published_diagnostic(&self, uri: &str) -> Option<PublishedDiagnostic> {
        self.server.published_diagnostic(uri)
    }

    async fn wait_for_publish(
        &self,
        documents: &[OpenDocument],
        wait: Duration,
    ) -> Vec<(OpenDocument, Value)> {
        self.server.wait_for_publish(documents, wait).await
    }

    fn store_pull(&self, uri: &str, value: Value) {
        self.server.store_pull(uri, value);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LspConnectionInfo {
    pub language: String,
    pub display_language: String,
    pub server: Option<String>,
    pub server_version: Option<String>,
    pub capabilities: LspCapabilities,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenDocument {
    pub uri: String,
    pub version: i64,
}

/// Typed low-level failures. Keeping the JSON-RPC code lets callers distinguish
/// "method not supported" from a transport crash without parsing strings.
#[derive(Debug, Clone, Error)]
pub(crate) enum LspError {
    #[error("language server is not running")]
    NotRunning,
    #[error("language server transport error: {0}")]
    Transport(String),
    #[error("language server did not respond to {method} within {secs}s")]
    Timeout { method: String, secs: u64 },
    #[error("language server exited during {method}")]
    Closed { method: String },
    #[error("language server error {code}: {message}")]
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("language server protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Message(String),
}

impl LspError {
    pub(crate) fn is_transport(&self) -> bool {
        matches!(
            self,
            Self::NotRunning | Self::Transport(_) | Self::Closed { .. }
        )
    }

    pub(crate) fn is_method_not_found(&self) -> bool {
        matches!(self, Self::Rpc { code: -32601, .. })
    }

    pub(crate) fn status(&self) -> Status {
        match self {
            Self::Timeout { .. } => Status::Timeout,
            Self::NotRunning | Self::Transport(_) | Self::Closed { .. } => Status::Crashed,
            Self::Rpc { code: -32601, .. } => Status::Unsupported,
            _ => Status::Unavailable,
        }
    }

    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "lsp_timeout",
            Self::NotRunning | Self::Transport(_) | Self::Closed { .. } => "lsp_crashed",
            Self::Rpc { code: -32601, .. } => "lsp_unsupported_method",
            Self::Rpc { .. } => "lsp_error",
            _ => "lsp_error",
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "LSP_TIMEOUT",
            Self::NotRunning | Self::Transport(_) | Self::Closed { .. } => "LSP_CRASHED",
            Self::Rpc { code: -32601, .. } => "LSP_METHOD_UNAVAILABLE",
            Self::Rpc { .. } => "LSP_ERROR",
            _ => "LSP_ERROR",
        }
    }
}

impl From<std::io::Error> for LspError {
    fn from(error: std::io::Error) -> Self {
        Self::Transport(error.to_string())
    }
}

impl From<serde_json::Error> for LspError {
    fn from(error: serde_json::Error) -> Self {
        Self::Protocol(error.to_string())
    }
}

#[derive(Debug, Clone, Error)]
pub(crate) enum LspConnectError {
    #[error("no {display} language server found on PATH; searched {searched}; {hint}")]
    NotInstalled {
        language: String,
        display: String,
        searched: String,
        hint: String,
    },
    #[error("language server {program} for {display} failed to start: {message}")]
    StartFailed {
        language: String,
        display: String,
        program: String,
        message: String,
    },
    #[error(
        "language server for {display} is cooling down after {message}; retry in {retry_after_secs}s"
    )]
    Cooldown {
        language: String,
        display: String,
        retry_after_secs: u64,
        message: String,
    },
}

impl LspConnectError {
    #[cfg(test)]
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NotInstalled { .. } | Self::Cooldown { .. } => "LSP_UNAVAILABLE",
            Self::StartFailed { .. } => "LSP_START_FAILED",
        }
    }

    #[cfg(test)]
    pub(crate) fn install_hint(&self) -> Option<String> {
        match self {
            Self::NotInstalled { hint, .. } => Some(hint.clone()),
            Self::StartFailed { .. } | Self::Cooldown { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn searched(&self) -> Vec<String> {
        match self {
            Self::NotInstalled { searched, .. } => searched
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect(),
            Self::StartFailed { program, .. } => vec![program.clone()],
            Self::Cooldown { .. } => Vec::new(),
        }
    }
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
    drop(servers);
    if let Ok(mut failures) = connect_failures().lock() {
        failures.retain(|key, _| key.root != root);
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
            let _ = sender.send(Err(LspError::Closed {
                method: "shutdown".to_string(),
            }));
        }
    }
    server.diagnostics_notify.notify_waiters();
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
            idle.then_some(key.clone())
        })
        .collect::<Vec<_>>();
    for key in stale {
        if let Some(server) = servers.remove(&key) {
            kill_server(&server);
        }
    }
}

async fn start_server(
    context: &ToolContext,
    key: &ServerKey,
    language: LspLanguage,
    program: &str,
    args: &[String],
) -> Result<SharedServer> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    start_server_with(context, key, language, program, &args).await
}

async fn get_server(
    context: &ToolContext,
    language: LspLanguage,
) -> Result<SharedServer, LspConnectError> {
    reap_idle_servers();
    let root = context
        .root
        .canonicalize()
        .map_err(|error| LspConnectError::StartFailed {
            language: language.id.to_string(),
            display: language.display.to_string(),
            program: "<workspace>".to_string(),
            message: error.to_string(),
        })?;
    let key = ServerKey {
        root: root.clone(),
        language: language.id.to_string(),
    };
    if let Some((retry_after_secs, message)) = cooldown_for(&key) {
        return Err(LspConnectError::Cooldown {
            language: language.id.to_string(),
            display: language.display.to_string(),
            retry_after_secs,
            message,
        });
    }
    let mut previous = None;
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
        previous = Some((server.program.clone(), server.args.clone()));
        if let Ok(mut servers) = registry().lock() {
            servers.remove(&key);
        }
        kill_server(&server);
    }
    let discovered = discover_server(language).map(|(program, args)| {
        (
            program,
            args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>(),
        )
    });
    let (program, args) = discovered
        .or(previous)
        .ok_or_else(|| LspConnectError::NotInstalled {
            language: language.id.to_string(),
            display: language.display.to_string(),
            searched: language
                .candidates
                .iter()
                .map(|(program, _)| *program)
                .collect::<Vec<_>>()
                .join(", "),
            hint: language.install_hint.to_string(),
        })?;
    let server = match start_server(context, &key, language, &program, &args).await {
        Ok(server) => server,
        Err(error) => {
            let message = error.to_string();
            record_connect_failure(&key, &message);
            return Err(LspConnectError::StartFailed {
                language: language.id.to_string(),
                display: language.display.to_string(),
                program,
                message,
            });
        }
    };
    clear_connect_failure(&key);
    if let Ok(mut servers) = registry().lock() {
        servers.insert(key, server.clone());
    }
    Ok(server)
}

async fn restart_language(
    context: &ToolContext,
    language: LspLanguage,
) -> Result<LspConnection, LspConnectError> {
    let root = context
        .root
        .canonicalize()
        .map_err(|error| LspConnectError::StartFailed {
            language: language.id.to_string(),
            display: language.display.to_string(),
            program: "<workspace>".to_string(),
            message: error.to_string(),
        })?;
    let key = ServerKey {
        root,
        language: language.id.to_string(),
    };
    let previous = registry()
        .lock()
        .ok()
        .and_then(|mut servers| servers.remove(&key))
        .map(|server| {
            let previous = (server.program.clone(), server.args.clone());
            kill_server(&server);
            previous
        });
    let discovered = discover_server(language).map(|(program, args)| {
        (
            program,
            args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>(),
        )
    });
    let (program, args) = discovered
        .or(previous)
        .ok_or_else(|| LspConnectError::NotInstalled {
            language: language.id.to_string(),
            display: language.display.to_string(),
            searched: language
                .candidates
                .iter()
                .map(|(program, _)| *program)
                .collect::<Vec<_>>()
                .join(", "),
            hint: language.install_hint.to_string(),
        })?;
    let server = start_server(context, &key, language, &program, &args)
        .await
        .map_err(|error| LspConnectError::StartFailed {
            language: language.id.to_string(),
            display: language.display.to_string(),
            program: program.clone(),
            message: error.to_string(),
        })?;
    clear_connect_failure(&key);
    if let Ok(mut servers) = registry().lock() {
        servers.insert(key, server.clone());
    }
    Ok(LspConnection { server })
}

pub(crate) async fn connect(
    context: &ToolContext,
    language: LspLanguage,
) -> Result<LspConnection, LspConnectError> {
    get_server(context, language)
        .await
        .map(|server| LspConnection { server })
}

pub(crate) fn running_connection(
    context: &ToolContext,
    language: LspLanguage,
) -> Option<LspConnection> {
    let root = context.root.canonicalize().ok()?;
    let key = ServerKey {
        root,
        language: language.id.to_string(),
    };
    let server = registry().lock().ok()?.get(&key).cloned()?;
    if !server.alive.load(Ordering::SeqCst) {
        return None;
    }
    if let Ok(mut last) = server.last_used.lock() {
        *last = Instant::now();
    }
    Some(LspConnection { server })
}

#[allow(dead_code)]
fn disconnect(context: &ToolContext, language: LspLanguage) {
    let Ok(root) = context.root.canonicalize() else {
        return;
    };
    let key = ServerKey {
        root,
        language: language.id.to_string(),
    };
    if let Ok(mut servers) = registry().lock()
        && let Some(server) = servers.remove(&key)
    {
        kill_server(&server);
    }
}

async fn start_server_with(
    context: &ToolContext,
    key: &ServerKey,
    language: LspLanguage,
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
    let diagnostics_notify = Arc::new(Notify::new());
    let pull_cache = Arc::new(Mutex::new(HashMap::new()));
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
    let reader_notify = diagnostics_notify.clone();
    let reader_alive = alive.clone();
    let reader_stdin = stdin.clone();
    let reader_root_uri = root_uri.clone();
    tokio::spawn(reader_loop(
        stdout,
        reader_stdin,
        reader_pending,
        reader_diagnostics,
        reader_notify,
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
    let mut server = LspServer {
        key: key.clone(),
        language,
        program: program.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        pid,
        stdin,
        pending,
        next_id: AtomicI64::new(1),
        diagnostics,
        diagnostics_notify,
        pull_cache,
        open_docs,
        alive,
        capabilities: LspCapabilities::default(),
        server_info: None,
        last_used: Mutex::new(Instant::now()),
    };
    let init_params = json!({
        "processId": null,
        "clientInfo": {"name": "VerlinPark", "version": "0.1"},
        "rootUri": root_uri,
        "capabilities": {
            "workspace": {
                "workspaceFolders": true,
                "configuration": true,
                "workspaceEdit": {"documentChanges": true},
                "symbol": {"symbolKind": {"valueSet": [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26]}}
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
    let initialized = match initialized {
        Ok(value) => value,
        Err(error) => {
            kill_server(&server);
            return Err(anyhow!("language server initialize failed: {error}"));
        }
    };
    server.capabilities = LspCapabilities::from_initialize(&initialized);
    server.server_info = initialized.get("serverInfo").and_then(|info| {
        info.get("name")
            .and_then(Value::as_str)
            .map(|name| LspServerInfo {
                name: name.to_string(),
                version: info
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
    });
    if let Err(error) = server.notify("initialized", json!({})).await {
        kill_server(&server);
        return Err(anyhow!("sending initialized notification: {error}"));
    }
    Ok(Arc::new(server))
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
    async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
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
    ) -> Result<Value, LspError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(LspError::NotRunning);
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
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(LspError::Closed {
                method: method.to_string(),
            }),
            Err(_) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                // Best-effort cancellation so a slow server stops work it can
                // no longer deliver a useful answer for. The request is still
                // reported as timed out and is never replayed.
                let _ = self.notify("$/cancelRequest", json!({"id": id})).await;
                Err(LspError::Timeout {
                    method: method.to_string(),
                    secs: wait.as_secs(),
                })
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        write_message(&self.stdin, &message).await
    }

    async fn open_or_update_versioned(&self, path: &Path) -> Result<OpenDocument, LspError> {
        let canonical = resolve_within_root(&self.key.root, path).ok_or_else(|| {
            LspError::Message("LSP document path escapes the workspace root".to_string())
        })?;
        let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
            LspError::Message(format!("reading {} for LSP: {error}", canonical.display()))
        })?;
        if metadata.len() > MAX_LSP_DOCUMENT_BYTES {
            return Err(LspError::Message(format!(
                "{} is larger than the {} MiB LSP document cap",
                canonical.display(),
                MAX_LSP_DOCUMENT_BYTES / 1024 / 1024
            )));
        }
        let uri = file_uri(&canonical)?;
        let text = tokio::fs::read_to_string(&canonical)
            .await
            .with_context(|| format!("reading {} for LSP", canonical.display()))
            .map_err(|error| LspError::Message(error.to_string()))?;
        let (was_open, version) = {
            let mut docs = self
                .open_docs
                .lock()
                .map_err(|_| LspError::Message("LSP document map lock poisoned".to_string()))?;
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
        Ok(OpenDocument { uri, version })
    }

    fn published_diagnostic(&self, uri: &str) -> Option<PublishedDiagnostic> {
        self.diagnostics
            .lock()
            .ok()
            .and_then(|diagnostics| diagnostics.get(uri).cloned())
            .filter(|diagnostic| diagnostic.received.elapsed() < Duration::from_secs(LSP_IDLE_SECS))
    }

    async fn wait_for_publish(
        &self,
        documents: &[OpenDocument],
        wait: Duration,
    ) -> Vec<(OpenDocument, Value)> {
        let deadline = Instant::now() + wait;
        loop {
            let found = documents
                .iter()
                .filter_map(|document| {
                    let diagnostic = self.published_diagnostic(&document.uri)?;
                    let fresh = match diagnostic.version {
                        Some(version) => version >= document.version,
                        None => diagnostic.received.elapsed() < Duration::from_secs(30),
                    };
                    fresh.then_some((document.clone(), diagnostic.params))
                })
                .collect::<Vec<_>>();
            if found.len() == documents.len() || Instant::now() >= deadline {
                return found;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if timeout(remaining, self.diagnostics_notify.notified())
                .await
                .is_err()
            {
                return found;
            }
        }
    }

    #[allow(dead_code)]
    fn cached_pull(&self, uri: &str) -> Option<Value> {
        self.pull_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(uri).cloned())
            .filter(|cached| cached.received.elapsed() < Duration::from_secs(30))
            .map(|cached| cached.value)
    }

    fn store_pull(&self, uri: &str, value: Value) {
        if let Ok(mut cache) = self.pull_cache.lock() {
            if cache.len() >= MAX_CACHED_DIAGNOSTIC_URIS && !cache.contains_key(uri) {
                cache.clear();
            }
            cache.insert(
                uri.to_string(),
                PulledDiagnostic {
                    value,
                    received: Instant::now(),
                },
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reader_loop(
    stdout: tokio::process::ChildStdout,
    stdin: Arc<AsyncMutex<ChildStdin>>,
    pending: PendingMap,
    diagnostics: DiagnosticsMap,
    diagnostics_notify: Arc<Notify>,
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
                        Err(LspError::Rpc {
                            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                            message: error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("language server error")
                                .to_string(),
                            data: error.get("data").cloned(),
                        })
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
                    "window/workDoneProgress/create" => Value::Null,
                    "window/showMessageRequest" => Value::Null,
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
        {
            let version = params.get("version").and_then(Value::as_i64);
            let diagnostic = PublishedDiagnostic {
                params: params.clone(),
                version,
                received: Instant::now(),
            };
            if let Ok(mut diagnostics) = diagnostics.lock() {
                let stale = diagnostics
                    .get(uri)
                    .and_then(|existing| existing.version)
                    .zip(version)
                    .is_some_and(|(existing, new)| new < existing);
                if !stale {
                    if diagnostics.len() >= MAX_CACHED_DIAGNOSTIC_URIS
                        && !diagnostics.contains_key(uri)
                    {
                        diagnostics.clear();
                    }
                    let item_count = params
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    if item_count <= MAX_CACHED_DIAGNOSTICS_PER_URI {
                        diagnostics.insert(uri.to_string(), diagnostic);
                    }
                }
            }
            diagnostics_notify.notify_waiters();
        }
    }
    alive.store(false, Ordering::SeqCst);
    diagnostics_notify.notify_waiters();
    if let Ok(mut pending) = pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(LspError::Closed {
                method: "reader".to_string(),
            }));
        }
    }
}

async fn write_message(
    stdin: &Arc<AsyncMutex<ChildStdin>>,
    message: &Value,
) -> Result<(), LspError> {
    let body = serde_json::to_vec(message)?;
    if body.len() > MAX_LSP_MESSAGE_BYTES {
        return Err(LspError::Message("LSP message exceeds 16 MiB".to_string()));
    }
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut stdin = stdin.lock().await;
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Value, LspError> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Err(LspError::Transport(
                "language server closed stdout".to_string(),
            ));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            let value = value
                .trim()
                .parse::<usize>()
                .map_err(|error| LspError::Protocol(format!("invalid Content-Length: {error}")))?;
            if value > MAX_LSP_MESSAGE_BYTES {
                return Err(LspError::Protocol(
                    "language server sent an oversized message".to_string(),
                ));
            }
            content_length = Some(value);
        }
    }
    let length = content_length
        .ok_or_else(|| LspError::Protocol("LSP message has no Content-Length".to_string()))?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

fn file_uri(path: &Path) -> Result<String, LspError> {
    reqwest::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| {
            LspError::Message(format!(
                "path cannot be converted to a file URI: {}",
                path.display()
            ))
        })
}

/// Resolve a path and prove it is inside the workspace root. Existing symlinks
/// are canonicalized, so a link pointing outside the worktree is rejected.
fn resolve_within_root(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved = if candidate.exists() {
        candidate.canonicalize().ok()?
    } else {
        let parent = candidate.parent()?.canonicalize().ok()?;
        parent.join(candidate.file_name()?)
    };
    resolved.starts_with(&root).then_some(resolved)
}

fn uri_to_path(root: &Path, uri: &str) -> Option<PathBuf> {
    let url = reqwest::Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    let path = url.to_file_path().ok()?;
    resolve_within_root(root, &path)
}

fn relative_path(root: &Path, path: &Path) -> String {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.strip_prefix(&canonical_root)
        .or_else(|_| path.strip_prefix(root))
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// Convert a 1-based character column into the 0-based offset used by the
/// server's negotiated position encoding. UTF-16 is the LSP default; UTF-8 and
/// UTF-32 are supported because some servers negotiate them.
fn position_for(
    path: &Path,
    line: u64,
    column: u64,
    encoding: PositionEncoding,
) -> Result<(u64, u64)> {
    if line == 0 || column == 0 {
        bail!("line and column are 1-based and must be positive");
    }
    let text = std::fs::read_to_string(path)?;
    let line_text = text
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .ok_or_else(|| anyhow!("line {line} is beyond the end of {}", path.display()))?;
    let character = match encoding {
        PositionEncoding::Utf16 => line_text
            .chars()
            .take(column.saturating_sub(1) as usize)
            .map(char::len_utf16)
            .sum::<usize>(),
        PositionEncoding::Utf8 => line_text
            .chars()
            .take(column.saturating_sub(1) as usize)
            .map(char::len_utf8)
            .sum::<usize>(),
        PositionEncoding::Utf32 => column.saturating_sub(1) as usize,
    };
    Ok((line - 1, character as u64))
}

/// Convert a 0-based server offset back to a 1-based character column. A
/// position inside a multi-unit character resolves to that character rather
/// than a bogus column past the end of the line.
fn character_column(line: &str, offset: u64, encoding: PositionEncoding) -> u64 {
    let target = usize::try_from(offset).unwrap_or(usize::MAX);
    if encoding == PositionEncoding::Utf32 {
        let count = line.chars().count();
        return (target.min(count) as u64).saturating_add(1);
    }
    let mut units = 0usize;
    let mut characters = 0usize;
    for ch in line.chars() {
        if units >= target {
            break;
        }
        units = units.saturating_add(match encoding {
            PositionEncoding::Utf16 => ch.len_utf16(),
            PositionEncoding::Utf8 => ch.len_utf8(),
            PositionEncoding::Utf32 => 1,
        });
        characters = characters.saturating_add(1);
    }
    (characters as u64).saturating_add(1)
}

fn raw_range_parts(range: &Value) -> Option<(u64, u64, u64, u64)> {
    Some((
        range.get("start")?.get("line")?.as_u64()?,
        range.get("start")?.get("character")?.as_u64()?,
        range.get("end")?.get("line")?.as_u64()?,
        range.get("end")?.get("character")?.as_u64()?,
    ))
}

#[derive(Debug)]
struct SourceCache {
    files: HashMap<PathBuf, Option<Vec<String>>>,
    encoding: PositionEncoding,
}

impl SourceCache {
    #[cfg(test)]
    fn new(_root: &Path) -> Self {
        Self::with_encoding(_root, PositionEncoding::Utf16)
    }

    fn with_encoding(_root: &Path, encoding: PositionEncoding) -> Self {
        Self {
            files: HashMap::new(),
            encoding,
        }
    }

    fn lines(&mut self, path: &Path) -> Option<&[String]> {
        let key = path.to_path_buf();
        if !self.files.contains_key(&key) {
            let loaded = std::fs::metadata(&key)
                .ok()
                .filter(|metadata| metadata.len() <= MAX_LSP_DOCUMENT_BYTES)
                .and_then(|_| std::fs::read_to_string(&key).ok())
                .map(|text| text.split('\n').map(str::to_string).collect::<Vec<_>>());
            self.files.insert(key.clone(), loaded);
        }
        self.files.get(&key).and_then(|lines| lines.as_deref())
    }

    fn map_range(&mut self, path: &Path, range: &Value) -> Option<(CodeRange, String)> {
        let (start_line, start_character, end_line, end_character) = raw_range_parts(range)?;
        let encoding = self.encoding;
        let lines = self.lines(path)?;
        if lines.is_empty() {
            return None;
        }
        let last = lines.len().saturating_sub(1);
        let start_index = usize::try_from(start_line).ok()?.min(last);
        let end_index = usize::try_from(end_line).ok()?.min(last);
        let start_text = lines.get(start_index)?.trim_end_matches('\r');
        let end_text = lines.get(end_index)?.trim_end_matches('\r');
        let mapped = CodeRange {
            line: (start_index as u64).saturating_add(1),
            column: character_column(start_text, start_character, encoding),
            end_line: (end_index as u64).saturating_add(1),
            end_column: character_column(end_text, end_character, encoding),
        };
        let context = start_text.trim().chars().take(300).collect::<String>();
        Some((mapped, context))
    }
}

fn location_from_value(
    cache: &mut SourceCache,
    root: &Path,
    value: &Value,
    kind: Option<&str>,
) -> Option<CodeLocation> {
    let uri = value
        .get("uri")
        .or_else(|| value.get("targetUri"))
        .and_then(Value::as_str)?;
    let path = uri_to_path(root, uri)?;
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))
        .or_else(|| value.get("targetRange"))?;
    let (mapped, context) = cache.map_range(&path, range)?;
    Some(CodeLocation {
        path: relative_path(root, &path),
        line: mapped.line,
        column: mapped.column,
        end_line: mapped.end_line,
        end_column: mapped.end_column,
        range: mapped,
        context: Some(context),
        is_definition: false,
        kind: kind.map(str::to_string),
        source: "lsp".to_string(),
        confidence: "lsp".to_string(),
        confidence_score: LSP_CONFIDENCE_REFERENCE,
    })
}

fn locations_from_value(
    cache: &mut SourceCache,
    root: &Path,
    value: &Value,
    limit: usize,
) -> Vec<CodeLocation> {
    let values = if let Some(array) = value.as_array() {
        array.iter().collect::<Vec<_>>()
    } else if value.is_object() {
        vec![value]
    } else {
        Vec::new()
    };
    values
        .into_iter()
        .filter_map(|location| location_from_value(cache, root, location, None))
        .take(limit)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn symbol_from_kind(
    name: &str,
    kind_number: u64,
    path: &str,
    language: &str,
    range: CodeRange,
    container: Option<String>,
    signature: String,
    score: f64,
) -> CodeSymbol {
    let kind = symbol_kind(kind_number).to_string();
    CodeSymbol {
        id: format!("{path}:{}:{name}", range.line),
        name: name.to_string(),
        kind_group: kind_group(&kind).to_string(),
        kind,
        language: language.to_string(),
        path: path.to_string(),
        line: range.line,
        column: range.column,
        end_line: range.end_line,
        end_column: range.end_column,
        range,
        container,
        visibility: None,
        signature,
        score,
        source: "lsp".to_string(),
        confidence: "lsp".to_string(),
        confidence_score: LSP_CONFIDENCE_SYMBOL,
    }
}

fn document_symbols_from_value(
    value: &Value,
    cache: &mut SourceCache,
    root: &Path,
    file: &Path,
    language: &str,
    limit: usize,
) -> Vec<CodeSymbol> {
    let mut output = Vec::new();
    if let Some(array) = value.as_array() {
        for symbol in array {
            collect_document_symbol(
                symbol,
                None,
                cache,
                root,
                file,
                language,
                &mut output,
                limit,
            );
            if output.len() >= limit {
                break;
            }
        }
    }
    output
}

#[allow(clippy::too_many_arguments)]
fn collect_document_symbol(
    symbol: &Value,
    container: Option<&str>,
    cache: &mut SourceCache,
    root: &Path,
    file: &Path,
    language: &str,
    output: &mut Vec<CodeSymbol>,
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
    let path = if let Some(uri) = symbol
        .get("location")
        .and_then(|location| location.get("uri"))
        .and_then(Value::as_str)
    {
        let Some(path) = uri_to_path(root, uri) else {
            return;
        };
        path
    } else {
        file.to_path_buf()
    };
    let range_value = symbol
        .get("selectionRange")
        .or_else(|| {
            symbol
                .get("location")
                .and_then(|location| location.get("range"))
        })
        .or_else(|| symbol.get("range"));
    if let Some(range_value) = range_value
        && let Some((mut range, context)) = cache.map_range(&path, range_value)
    {
        if let Some(full_range) = symbol
            .get("range")
            .and_then(|value| cache.map_range(&path, value))
        {
            range.end_line = full_range.0.end_line;
            range.end_column = full_range.0.end_column;
        }
        let signature = context.trim().chars().take(240).collect::<String>();
        output.push(symbol_from_kind(
            &name,
            symbol.get("kind").and_then(Value::as_u64).unwrap_or(0),
            &relative_path(root, &path),
            language,
            range,
            container.map(str::to_string),
            signature,
            0.0,
        ));
    }
    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            collect_document_symbol(
                child,
                Some(&name),
                cache,
                root,
                file,
                language,
                output,
                limit,
            );
            if output.len() >= limit {
                break;
            }
        }
    }
}

fn workspace_symbols_from_value(
    value: &Value,
    cache: &mut SourceCache,
    root: &Path,
    language: &str,
    limit: usize,
) -> Vec<CodeSymbol> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|symbol| {
            let location = symbol.get("location")?;
            let uri = location.get("uri").and_then(Value::as_str)?;
            let path = uri_to_path(root, uri)?;
            let range_value = location.get("range")?;
            let (range, context) = cache.map_range(&path, range_value)?;
            let name = symbol.get("name").and_then(Value::as_str).unwrap_or("?");
            Some(symbol_from_kind(
                name,
                symbol.get("kind").and_then(Value::as_u64).unwrap_or(0),
                &relative_path(root, &path),
                language,
                range,
                symbol
                    .get("containerName")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                context.trim().chars().take(240).collect(),
                0.0,
            ))
        })
        .take(limit)
        .collect()
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

fn diagnostic_severity(severity: Option<u64>) -> &'static str {
    match severity {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "information",
        Some(4) => "hint",
        _ => "error",
    }
}

fn diagnostics_from_items(
    items: &[Value],
    cache: &mut SourceCache,
    root: &Path,
    fallback_path: Option<&Path>,
    server: Option<&str>,
    limit: usize,
) -> Vec<CodeDiagnostic> {
    items
        .iter()
        .filter_map(|diagnostic| {
            let range_value = diagnostic.get("range")?;
            let path = diagnostic
                .get("uri")
                .and_then(Value::as_str)
                .and_then(|uri| uri_to_path(root, uri))
                .or_else(|| fallback_path.map(Path::to_path_buf))?;
            let (range, _) = cache.map_range(&path, range_value)?;
            Some(CodeDiagnostic {
                path: relative_path(root, &path),
                line: range.line,
                column: range.column,
                end_line: range.end_line,
                end_column: range.end_column,
                range,
                severity: diagnostic_severity(diagnostic.get("severity").and_then(Value::as_u64))
                    .to_string(),
                code: diagnostic.get("code").cloned(),
                source: diagnostic
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                server: server.map(str::to_string),
                message: diagnostic
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(2_000)
                    .collect(),
                backend: "lsp".to_string(),
                confidence_score: LSP_CONFIDENCE_DIAGNOSTIC,
            })
        })
        .take(limit)
        .collect()
}

fn diagnostics_from_pull(
    value: &Value,
    cache: &mut SourceCache,
    root: &Path,
    path: &Path,
    server: Option<&str>,
    limit: usize,
) -> Vec<CodeDiagnostic> {
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| diagnostics_from_items(items, cache, root, Some(path), server, limit))
        .unwrap_or_default()
}

fn diagnostics_from_publish(
    value: &Value,
    cache: &mut SourceCache,
    root: &Path,
    path: &Path,
    server: Option<&str>,
    limit: usize,
) -> Vec<CodeDiagnostic> {
    value
        .get("diagnostics")
        .and_then(Value::as_array)
        .map(|items| diagnostics_from_items(items, cache, root, Some(path), server, limit))
        .unwrap_or_default()
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

#[derive(Debug, Clone)]
pub(crate) struct DeclarationPosition {
    pub path: PathBuf,
    pub line: u64,
    pub column: u64,
    pub kind: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolScan {
    pub symbols: Vec<CodeSymbol>,
    pub method: String,
    pub status: Status,
}

#[derive(Debug, Clone)]
pub(crate) struct ReferenceScan {
    pub locations: Vec<CodeLocation>,
    pub declaration: Option<CodeLocation>,
    pub method: String,
    pub status: Status,
    pub truncated: bool,
    pub server: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticScan {
    pub diagnostics: Vec<CodeDiagnostic>,
    pub method: String,
    pub status: Status,
    pub truncated: bool,
    pub server: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolBatch {
    pub symbols: Vec<CodeSymbol>,
    pub attempts: Vec<LspAttempt>,
    pub status: Status,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ReferenceBatch {
    pub locations: Vec<CodeLocation>,
    pub declaration: Option<CodeLocation>,
    pub attempts: Vec<LspAttempt>,
    pub status: Status,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticBatch {
    pub diagnostics: Vec<CodeDiagnostic>,
    pub attempts: Vec<LspAttempt>,
    pub status: Status,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FileSymbolBatch {
    pub symbols: Vec<CodeSymbol>,
    pub attempts: Vec<LspAttempt>,
    pub status: Status,
}

#[derive(Debug, Clone)]
pub(crate) struct LspAttempt {
    pub language: String,
    pub server: Option<String>,
    pub server_version: Option<String>,
    pub method: Option<String>,
    pub status: Status,
    pub message: Option<String>,
    pub install_hint: Option<String>,
}

impl LspAttempt {
    fn success(language: LspLanguage, info: &LspConnectionInfo, method: &str) -> Self {
        Self {
            language: language.id.to_string(),
            server: info.server.clone(),
            server_version: info.server_version.clone(),
            method: Some(method.to_string()),
            status: Status::Ok,
            message: None,
            install_hint: None,
        }
    }

    fn failure(language: LspLanguage, error: &LspAttemptError) -> Self {
        Self {
            language: language.id.to_string(),
            server: error.server().map(str::to_string),
            server_version: None,
            method: error.method().map(str::to_string),
            status: error.status(),
            message: Some(error.to_string()),
            install_hint: error.install_hint(),
        }
    }

    pub(crate) fn to_backend_attempt(&self) -> BackendAttempt {
        BackendAttempt {
            language: self.language.clone(),
            server: self.server.clone(),
            method: self.method.clone(),
            status: self.status.as_str().to_string(),
            message: self.message.clone(),
            install_hint: self.install_hint.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum LspAttemptError {
    Unavailable {
        language: String,
        display: String,
        message: String,
        install_hint: Option<String>,
        searched: Vec<String>,
    },
    Unsupported {
        language: String,
        display: String,
        server: Option<String>,
        method: String,
        capabilities: LspCapabilities,
    },
    Failed {
        language: String,
        display: String,
        server: Option<String>,
        error: Box<LspError>,
    },
    NoLanguage {
        supported: Vec<String>,
    },
}

impl LspAttemptError {
    pub(crate) fn status(&self) -> Status {
        match self {
            Self::Unavailable { .. } | Self::NoLanguage { .. } => Status::Unavailable,
            Self::Unsupported { .. } => Status::Unsupported,
            Self::Failed { error, .. } => error.status(),
        }
    }

    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "lsp_unavailable",
            Self::Unsupported { .. } => "lsp_unsupported_method",
            Self::Failed { error, .. } => error.reason(),
            Self::NoLanguage { .. } => "lsp_unsupported_language",
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Unavailable { .. } => "LSP_UNAVAILABLE",
            Self::Unsupported { .. } => "LSP_METHOD_UNAVAILABLE",
            Self::Failed { error, .. } => error.code(),
            Self::NoLanguage { .. } => "LSP_UNAVAILABLE",
        }
    }

    pub(crate) fn install_hint(&self) -> Option<String> {
        match self {
            Self::Unavailable { install_hint, .. } => install_hint.clone(),
            Self::Failed { .. } | Self::Unsupported { .. } | Self::NoLanguage { .. } => None,
        }
    }

    pub(crate) fn language(&self) -> Option<&str> {
        match self {
            Self::Unavailable { language, .. }
            | Self::Unsupported { language, .. }
            | Self::Failed { language, .. } => Some(language),
            Self::NoLanguage { .. } => None,
        }
    }

    pub(crate) fn server(&self) -> Option<&str> {
        match self {
            Self::Unsupported { server, .. } | Self::Failed { server, .. } => server.as_deref(),
            Self::Unavailable { .. } | Self::NoLanguage { .. } => None,
        }
    }

    pub(crate) fn method(&self) -> Option<&str> {
        match self {
            Self::Unsupported { method, .. } => Some(method),
            Self::Failed { .. } | Self::Unavailable { .. } | Self::NoLanguage { .. } => None,
        }
    }

    pub(crate) fn capabilities(&self) -> Option<&LspCapabilities> {
        match self {
            Self::Unsupported { capabilities, .. } => Some(capabilities),
            _ => None,
        }
    }

    pub(crate) fn searched(&self) -> Vec<String> {
        match self {
            Self::Unavailable { searched, .. } => searched.clone(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn attempt(&self) -> BackendAttempt {
        BackendAttempt {
            language: self.language().unwrap_or("unknown").to_string(),
            server: self.server().map(str::to_string),
            method: self.method().map(str::to_string),
            status: self.status().as_str().to_string(),
            message: Some(self.to_string()),
            install_hint: self.install_hint(),
        }
    }

    /// Prefer a missing-install explanation over a timeout from another
    /// language when several languages were attempted.
    pub(crate) fn merge(errors: Vec<LspAttemptError>) -> Self {
        if errors.is_empty() {
            return Self::NoLanguage {
                supported: all_languages()
                    .iter()
                    .map(|language| language.id.to_string())
                    .collect(),
            };
        }
        if let Some(error) = errors.iter().find(|error| {
            matches!(
                error,
                Self::Unavailable {
                    install_hint: Some(_),
                    ..
                }
            )
        }) {
            return error.clone();
        }
        if errors
            .iter()
            .all(|error| matches!(error, Self::NoLanguage { .. }))
        {
            return errors.into_iter().next().unwrap_or(Self::NoLanguage {
                supported: Vec::new(),
            });
        }
        errors.into_iter().next().unwrap_or(Self::NoLanguage {
            supported: Vec::new(),
        })
    }
}

impl std::fmt::Display for LspAttemptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable {
                display, message, ..
            } => write!(formatter, "{display}: {message}"),
            Self::Unsupported {
                display,
                server,
                method,
                ..
            } => write!(
                formatter,
                "{display} language server ({}) does not support {method}",
                server.as_deref().unwrap_or("unknown")
            ),
            Self::Failed { display, error, .. } => write!(formatter, "{display}: {error}"),
            Self::NoLanguage { supported } => write!(
                formatter,
                "no supported language server mapping; supported languages: {}",
                supported.join(", ")
            ),
        }
    }
}

impl std::error::Error for LspAttemptError {}

fn attempt_error_from_connect(_language: LspLanguage, error: LspConnectError) -> LspAttemptError {
    match error {
        LspConnectError::NotInstalled {
            language: id,
            display,
            hint,
            searched,
        } => LspAttemptError::Unavailable {
            language: id,
            display,
            message: "no language server is installed".to_string(),
            install_hint: Some(hint),
            searched: searched
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect(),
        },
        LspConnectError::StartFailed {
            language: id,
            display,
            program,
            message,
        } => LspAttemptError::Failed {
            language: id,
            display,
            server: Some(program),
            error: Box::new(LspError::Message(format!("failed to start: {message}"))),
        },
        LspConnectError::Cooldown {
            language: id,
            display,
            retry_after_secs,
            message,
        } => LspAttemptError::Unavailable {
            language: id,
            display,
            message: format!(
                "language server recently failed to start and is cooling down for {retry_after_secs}s: {message}"
            ),
            install_hint: None,
            searched: Vec::new(),
        },
    }
}

fn attempt_error_from_lsp(
    language: LspLanguage,
    info: &LspConnectionInfo,
    error: LspError,
) -> LspAttemptError {
    LspAttemptError::Failed {
        language: language.id.to_string(),
        display: language.display.to_string(),
        server: info.server.clone(),
        error: Box::new(error),
    }
}

async fn connect_for_language(
    context: &ToolContext,
    language: LspLanguage,
    allow_spawn: bool,
) -> Result<LspConnection, LspAttemptError> {
    if allow_spawn {
        return connect(context, language)
            .await
            .map_err(|error| attempt_error_from_connect(language, error));
    }
    running_connection(context, language).ok_or_else(|| LspAttemptError::Unavailable {
        language: language.id.to_string(),
        display: language.display.to_string(),
        message: "no running language server; auto enrichment does not spawn one".to_string(),
        install_hint: Some(language.install_hint.to_string()),
        searched: language
            .candidates
            .iter()
            .map(|(program, _)| (*program).to_string())
            .collect(),
    })
}

fn unsupported(language: LspLanguage, info: &LspConnectionInfo, method: &str) -> LspAttemptError {
    LspAttemptError::Unsupported {
        language: language.id.to_string(),
        display: language.display.to_string(),
        server: info.server.clone(),
        method: method.to_string(),
        capabilities: info.capabilities.clone(),
    }
}

pub(crate) async fn symbols_on_connection(
    connection: &LspConnection,
    query: &str,
    scope_files: &[PathBuf],
    limit: usize,
) -> Result<SymbolScan, LspError> {
    let info = connection.info();
    let root = connection.root();
    let mut cache = SourceCache::with_encoding(&root, info.capabilities.position_encoding);
    if info.capabilities.workspace_symbols {
        match connection
            .request("workspace/symbol", json!({"query": query}))
            .await
        {
            Ok(value) => {
                let mut symbols = workspace_symbols_from_value(
                    &value,
                    &mut cache,
                    &root,
                    &info.display_language,
                    MAX_SYMBOLS,
                );
                for symbol in &mut symbols {
                    symbol.score = score_name_match(&symbol.name, query);
                    symbol.language = info.display_language.clone();
                }
                if !query.trim().is_empty() {
                    symbols.retain(|symbol| symbol.score > 0.0);
                }
                let (symbols, truncated) = prepare_symbols(symbols, Some(query), limit);
                let _ = truncated;
                return Ok(SymbolScan {
                    symbols,
                    method: "workspace/symbol".to_string(),
                    status: Status::Ok,
                });
            }
            Err(error) if error.is_method_not_found() => {
                // Fall through to document symbols when a server advertises
                // workspace symbols but rejects the method.
            }
            Err(error) => return Err(error),
        }
    }
    if info.capabilities.document_symbols && !scope_files.is_empty() {
        let (symbols, truncated) =
            document_symbols_on_connection(connection, scope_files, MAX_SYMBOLS).await?;
        let symbols = symbols
            .into_iter()
            .filter(|symbol| query.trim().is_empty() || score_name_match(&symbol.name, query) > 0.0)
            .map(|mut symbol| {
                symbol.score = score_name_match(&symbol.name, query);
                symbol
            })
            .collect::<Vec<_>>();
        let (symbols, _) = prepare_symbols(symbols, Some(query), limit);
        let _ = truncated;
        return Ok(SymbolScan {
            symbols,
            method: "textDocument/documentSymbol".to_string(),
            status: Status::Partial,
        });
    }
    Err(LspError::Rpc {
        code: -32601,
        message: "workspace/symbol and textDocument/documentSymbol are unavailable".to_string(),
        data: None,
    })
}

async fn document_symbols_on_connection(
    connection: &LspConnection,
    files: &[PathBuf],
    limit: usize,
) -> Result<(Vec<CodeSymbol>, bool), LspError> {
    let info = connection.info();
    let root = connection.root();
    let mut cache = SourceCache::with_encoding(&root, info.capabilities.position_encoding);
    let mut symbols = Vec::new();
    let mut truncated = files.len() > MAX_DOCUMENT_SYMBOL_FILES;
    for file in files.iter().take(MAX_DOCUMENT_SYMBOL_FILES) {
        if symbols.len() >= limit {
            truncated = true;
            break;
        }
        let document = connection.open_document(file).await?;
        let value = connection
            .request(
                "textDocument/documentSymbol",
                json!({"textDocument": {"uri": document.uri}}),
            )
            .await?;
        let remaining = limit.saturating_sub(symbols.len());
        let mut file_symbols = document_symbols_from_value(
            &value,
            &mut cache,
            &root,
            file,
            &info.display_language,
            remaining,
        );
        for symbol in &mut file_symbols {
            symbol.language = info.display_language.clone();
        }
        symbols.extend(file_symbols);
    }
    let (mut symbols, capped) = prepare_symbols(symbols, None, limit);
    truncated |= capped;
    symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.column.cmp(&right.column))
    });
    Ok((symbols, truncated))
}

pub(crate) async fn references_on_connection(
    connection: &LspConnection,
    declaration: &DeclarationPosition,
    include_declaration: bool,
    limit: usize,
) -> Result<ReferenceScan, LspError> {
    let info = connection.info();
    let (line, character) = position_for(
        &declaration.path,
        declaration.line,
        declaration.column,
        info.capabilities.position_encoding,
    )
    .map_err(|error| LspError::Message(error.to_string()))?;
    let document = connection.open_document(&declaration.path).await?;
    let position = json!({"line": line, "character": character});
    let root = connection.root();
    let mut cache = SourceCache::with_encoding(&root, info.capabilities.position_encoding);
    let mut declaration_location = None;
    if info.capabilities.definition
        && let Ok(value) = connection
            .request(
                "textDocument/definition",
                json!({"textDocument": {"uri": document.uri}, "position": position}),
            )
            .await
    {
        declaration_location = locations_from_value(&mut cache, &root, &value, 1)
            .into_iter()
            .next();
        if let Some(location) = &mut declaration_location {
            location.is_definition = true;
            location.kind = declaration.kind.clone();
        }
    }
    if !info.capabilities.references {
        if let Some(location) = declaration_location {
            let (locations, truncated) = prepare_locations(vec![location.clone()], limit);
            return Ok(ReferenceScan {
                locations,
                declaration: Some(location),
                method: "textDocument/definition".to_string(),
                status: Status::Partial,
                truncated,
                server: info.server.clone(),
            });
        }
        return Err(LspError::Rpc {
            code: -32601,
            message: "textDocument/references is unavailable".to_string(),
            data: None,
        });
    }
    let value = connection
        .request(
            "textDocument/references",
            json!({
                "textDocument": {"uri": document.uri},
                "position": position,
                "context": {"includeDeclaration": include_declaration},
            }),
        )
        .await?;
    let mut locations = locations_from_value(&mut cache, &root, &value, MAX_LOCATIONS);
    for location in &mut locations {
        location.kind = declaration.kind.clone();
        location.confidence_score = LSP_CONFIDENCE_REFERENCE;
        if let Some(declaration_location) = &declaration_location {
            location.is_definition = location.path == declaration_location.path
                && location.line == declaration_location.line
                && location.column == declaration_location.column;
        }
    }
    let (locations, truncated) = prepare_locations(locations, limit);
    Ok(ReferenceScan {
        locations,
        declaration: declaration_location,
        method: "textDocument/references".to_string(),
        status: Status::Ok,
        truncated,
        server: info.server.clone(),
    })
}

pub(crate) async fn diagnostics_on_connection(
    connection: &LspConnection,
    files: &[PathBuf],
    limit: usize,
) -> Result<DiagnosticScan, LspError> {
    let info = connection.info();
    let root = connection.root();
    let deadline = Instant::now() + Duration::from_secs(LSP_DIAGNOSTIC_TOTAL_SECS);
    let mut cache = SourceCache::with_encoding(&root, info.capabilities.position_encoding);
    let mut documents = Vec::new();
    let mut diagnostics = Vec::new();
    let mut truncated = false;
    for file in files.iter().take(MAX_DIAGNOSTIC_FILES) {
        if Instant::now() >= deadline {
            truncated = true;
            break;
        }
        match connection.open_document(file).await {
            Ok(document) => documents.push((file.clone(), document)),
            Err(LspError::Message(_)) => continue,
            Err(error) => return Err(error),
        }
    }
    if documents.len() < files.len().min(MAX_DIAGNOSTIC_FILES) {
        truncated = true;
    }
    if info.capabilities.pull_diagnostics {
        let method = "textDocument/diagnostic".to_string();
        let mut pull_failed = false;
        for (path, document) in &documents {
            if Instant::now() >= deadline {
                truncated = true;
                break;
            }
            let value = connection
                .request_with_timeout(
                    "textDocument/diagnostic",
                    json!({
                        "textDocument": {"uri": document.uri},
                        "identifier": null,
                        "previousResultId": null,
                    }),
                    Duration::from_secs(LSP_DIAGNOSTIC_REQUEST_SECS),
                )
                .await;
            match value {
                Ok(value) => {
                    let remaining = limit.saturating_sub(diagnostics.len()).max(1);
                    diagnostics.extend(diagnostics_from_pull(
                        &value,
                        &mut cache,
                        &root,
                        path,
                        info.server.as_deref(),
                        remaining,
                    ));
                    connection.store_pull(&document.uri, value);
                }
                Err(error) if error.is_method_not_found() => {
                    pull_failed = true;
                    break;
                }
                Err(LspError::Timeout { .. }) => {
                    let summary = prepare_diagnostics(diagnostics, limit);
                    let status = if summary.total > 0 {
                        Status::Partial
                    } else {
                        Status::Timeout
                    };
                    return Ok(DiagnosticScan {
                        diagnostics: summary
                            .groups
                            .into_iter()
                            .flat_map(|group| group.diagnostics)
                            .collect(),
                        method,
                        status,
                        truncated: true,
                        server: info.server.clone(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        if !pull_failed {
            let summary = prepare_diagnostics(diagnostics, limit);
            let capped = summary.truncated;
            return Ok(DiagnosticScan {
                diagnostics: summary
                    .groups
                    .into_iter()
                    .flat_map(|group| group.diagnostics)
                    .collect(),
                method,
                status: if truncated || capped {
                    Status::Partial
                } else {
                    Status::Ok
                },
                truncated: truncated || capped,
                server: info.server.clone(),
            });
        }
        diagnostics.clear();
    }
    let method = "textDocument/publishDiagnostics".to_string();
    let mut missing = Vec::new();
    for (path, document) in &documents {
        if let Some(diagnostic) = connection.published_diagnostic(&document.uri) {
            let fresh = match diagnostic.version {
                Some(version) => version >= document.version,
                None => true,
            };
            if fresh {
                diagnostics.extend(diagnostics_from_publish(
                    &diagnostic.params,
                    &mut cache,
                    &root,
                    path,
                    info.server.as_deref(),
                    limit,
                ));
                continue;
            }
        }
        missing.push(document.clone());
    }
    let mut timed_out = false;
    if !missing.is_empty() {
        let published = connection
            .wait_for_publish(&missing, Duration::from_millis(LSP_DIAGNOSTIC_WAIT_MS))
            .await;
        if published.len() < missing.len() {
            timed_out = true;
        }
        for (document, value) in published {
            let path = documents
                .iter()
                .find(|(_, candidate)| candidate.uri == document.uri)
                .map(|(path, _)| path.clone())
                .unwrap_or_else(|| root.clone());
            diagnostics.extend(diagnostics_from_publish(
                &value,
                &mut cache,
                &root,
                &path,
                info.server.as_deref(),
                limit,
            ));
        }
    }
    let summary = prepare_diagnostics(diagnostics, limit);
    let capped = summary.truncated;
    let status = if timed_out {
        if summary.total > 0 {
            Status::Partial
        } else {
            Status::Timeout
        }
    } else if capped {
        Status::Partial
    } else {
        Status::Ok
    };
    Ok(DiagnosticScan {
        diagnostics: summary
            .groups
            .into_iter()
            .flat_map(|group| group.diagnostics)
            .collect(),
        method,
        status,
        truncated: timed_out || capped,
        server: info.server.clone(),
    })
}

async fn search_symbols_in_language(
    context: &ToolContext,
    language: LspLanguage,
    query: &str,
    scope_files: &[PathBuf],
    limit: usize,
    allow_spawn: bool,
) -> Result<(Vec<CodeSymbol>, LspAttempt), LspAttemptError> {
    let mut attempt = 0usize;
    loop {
        let connection = connect_for_language(context, language, allow_spawn).await?;
        let info = connection.info();
        let capabilities = info.capabilities.clone();
        if !capabilities.workspace_symbols
            && !(capabilities.document_symbols && !scope_files.is_empty())
        {
            return Err(unsupported(language, &info, "workspace/symbol"));
        }
        match symbols_on_connection(&connection, query, scope_files, limit).await {
            Ok(scan) => {
                return Ok((
                    scan.symbols,
                    LspAttempt {
                        language: language.id.to_string(),
                        server: info.server.clone(),
                        server_version: info.server_version.clone(),
                        method: Some(scan.method),
                        status: scan.status,
                        message: None,
                        install_hint: None,
                    },
                ));
            }
            Err(error) if error.is_transport() && attempt < RESTART_ATTEMPTS => {
                match restart_language(context, language).await {
                    Ok(_) => attempt += 1,
                    Err(error) => return Err(attempt_error_from_connect(language, error)),
                }
            }
            Err(error) => return Err(attempt_error_from_lsp(language, &info, error)),
        }
    }
}

async fn references_in_language(
    context: &ToolContext,
    language: LspLanguage,
    declaration: &DeclarationPosition,
    include_declaration: bool,
    limit: usize,
) -> Result<ReferenceScan, LspAttemptError> {
    let mut attempt = 0usize;
    loop {
        let connection = connect_for_language(context, language, true).await?;
        let info = connection.info();
        if !info.capabilities.references && !info.capabilities.definition {
            return Err(unsupported(language, &info, "textDocument/references"));
        }
        match references_on_connection(&connection, declaration, include_declaration, limit).await {
            Ok(mut scan) => {
                scan.locations = scan
                    .locations
                    .into_iter()
                    .map(|mut location| {
                        location.kind = declaration.kind.clone();
                        location
                    })
                    .collect();
                return Ok(scan);
            }
            Err(error) if error.is_transport() && attempt < RESTART_ATTEMPTS => {
                match restart_language(context, language).await {
                    Ok(_) => attempt += 1,
                    Err(error) => return Err(attempt_error_from_connect(language, error)),
                }
            }
            Err(error) => return Err(attempt_error_from_lsp(language, &info, error)),
        }
    }
}

async fn diagnostics_in_language(
    context: &ToolContext,
    language: LspLanguage,
    files: &[PathBuf],
    limit: usize,
) -> Result<DiagnosticScan, LspAttemptError> {
    let mut attempt = 0usize;
    loop {
        let connection = connect_for_language(context, language, true).await?;
        let info = connection.info();
        match diagnostics_on_connection(&connection, files, limit).await {
            Ok(scan) => return Ok(scan),
            Err(error) if error.is_transport() && attempt < RESTART_ATTEMPTS => {
                match restart_language(context, language).await {
                    Ok(_) => attempt += 1,
                    Err(error) => return Err(attempt_error_from_connect(language, error)),
                }
            }
            Err(error) => return Err(attempt_error_from_lsp(language, &info, error)),
        }
    }
}

pub(crate) async fn search_symbols(
    context: &ToolContext,
    languages: &[LspLanguage],
    query: &str,
    scope_files: &[PathBuf],
    limit: usize,
    allow_spawn: bool,
) -> Result<SymbolBatch, LspAttemptError> {
    if languages.is_empty() {
        return Err(LspAttemptError::NoLanguage {
            supported: all_languages()
                .iter()
                .map(|language| language.id.to_string())
                .collect(),
        });
    }
    let mut symbols = Vec::new();
    let mut attempts = Vec::new();
    let mut errors = Vec::new();
    let mut truncated = false;
    for language in languages.iter().take(MAX_LANGUAGES_PER_QUERY) {
        let files = scope_files
            .iter()
            .filter(|file| {
                language_for_path(file).is_some_and(|candidate| candidate.id == language.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        match search_symbols_in_language(context, *language, query, &files, limit, allow_spawn)
            .await
        {
            Ok((mut found, attempt)) => {
                symbols.append(&mut found);
                attempts.push(attempt);
            }
            Err(error) => {
                attempts.push(LspAttempt::failure(*language, &error));
                errors.push(error);
            }
        }
    }
    if attempts.iter().all(|attempt| !attempt.status.is_success()) {
        return Err(LspAttemptError::merge(errors));
    }
    let any_success = attempts.iter().any(|attempt| attempt.status.is_success());
    let any_failure = attempts.iter().any(|attempt| !attempt.status.is_success());
    let (symbols, capped) = prepare_symbols(symbols, Some(query), limit);
    truncated |= capped;
    Ok(SymbolBatch {
        symbols,
        status: if any_success && any_failure {
            Status::Partial
        } else {
            attempts
                .iter()
                .find(|attempt| attempt.status.is_success())
                .map(|attempt| attempt.status)
                .unwrap_or(Status::Partial)
        },
        attempts,
        truncated,
    })
}

pub(crate) async fn find_references(
    context: &ToolContext,
    declaration: &DeclarationPosition,
    include_declaration: bool,
    limit: usize,
) -> Result<ReferenceBatch, LspAttemptError> {
    let Some(language) = language_for_path(&declaration.path) else {
        return Err(LspAttemptError::NoLanguage {
            supported: all_languages()
                .iter()
                .map(|language| language.id.to_string())
                .collect(),
        });
    };
    match references_in_language(context, language, declaration, include_declaration, limit).await {
        Ok(scan) => {
            let attempt = LspAttempt {
                language: language.id.to_string(),
                server: scan.server.clone(),
                server_version: None,
                method: Some(scan.method.clone()),
                status: scan.status,
                message: None,
                install_hint: None,
            };
            Ok(ReferenceBatch {
                locations: scan.locations,
                declaration: scan.declaration,
                attempts: vec![attempt],
                status: scan.status,
                truncated: scan.truncated,
            })
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn collect_diagnostics(
    context: &ToolContext,
    files: &[PathBuf],
    limit: usize,
) -> Result<DiagnosticBatch, LspAttemptError> {
    let languages = languages_for_files(files, MAX_LANGUAGES_PER_QUERY);
    if languages.is_empty() {
        return Err(LspAttemptError::NoLanguage {
            supported: all_languages()
                .iter()
                .map(|language| language.id.to_string())
                .collect(),
        });
    }
    let mut diagnostics = Vec::new();
    let mut attempts = Vec::new();
    let mut errors = Vec::new();
    let mut truncated = false;
    for language in &languages {
        let scoped = files
            .iter()
            .filter(|file| {
                language_for_path(file).is_some_and(|candidate| candidate.id == language.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        match diagnostics_in_language(context, *language, &scoped, limit).await {
            Ok(scan) => {
                let success = scan.status.is_success();
                diagnostics.extend(scan.diagnostics);
                truncated |= scan.truncated;
                attempts.push(LspAttempt {
                    language: language.id.to_string(),
                    server: scan.server.clone(),
                    server_version: None,
                    method: Some(scan.method),
                    status: scan.status,
                    message: None,
                    install_hint: None,
                });
                if !success {
                    errors.push(LspAttemptError::Failed {
                        language: language.id.to_string(),
                        display: language.display.to_string(),
                        server: scan.server,
                        error: Box::new(LspError::Timeout {
                            method: "textDocument/publishDiagnostics".to_string(),
                            secs: LSP_DIAGNOSTIC_WAIT_MS / 1_000,
                        }),
                    });
                }
            }
            Err(error) => {
                attempts.push(LspAttempt::failure(*language, &error));
                errors.push(error);
            }
        }
    }
    if attempts.iter().all(|attempt| !attempt.status.is_success()) {
        return Err(LspAttemptError::merge(errors));
    }
    let summary = prepare_diagnostics(diagnostics, limit);
    let capped = summary.truncated;
    Ok(DiagnosticBatch {
        diagnostics: summary
            .groups
            .into_iter()
            .flat_map(|group| group.diagnostics)
            .collect(),
        status: if attempts.iter().any(|attempt| !attempt.status.is_success()) || capped {
            Status::Partial
        } else {
            Status::Ok
        },
        attempts,
        truncated: truncated || capped,
    })
}

pub(crate) async fn collect_file_symbols(
    context: &ToolContext,
    files: &[PathBuf],
    limit: usize,
    allow_spawn: bool,
) -> Result<FileSymbolBatch, LspAttemptError> {
    let languages = languages_for_files(files, MAX_LANGUAGES_PER_QUERY);
    if languages.is_empty() {
        return Err(LspAttemptError::NoLanguage {
            supported: all_languages()
                .iter()
                .map(|language| language.id.to_string())
                .collect(),
        });
    }
    let mut symbols = Vec::new();
    let mut attempts = Vec::new();
    let mut errors = Vec::new();
    let mut truncated = false;
    for language in &languages {
        let scoped = files
            .iter()
            .filter(|file| {
                language_for_path(file).is_some_and(|candidate| candidate.id == language.id)
            })
            .take(MAX_DOCUMENT_SYMBOL_FILES)
            .cloned()
            .collect::<Vec<_>>();
        let connection = match connect_for_language(context, *language, allow_spawn).await {
            Ok(connection) => connection,
            Err(error) => {
                attempts.push(LspAttempt::failure(*language, &error));
                errors.push(error);
                continue;
            }
        };
        let info = connection.info();
        if !info.capabilities.document_symbols {
            let error = unsupported(*language, &info, "textDocument/documentSymbol");
            attempts.push(LspAttempt::failure(*language, &error));
            errors.push(error);
            continue;
        }
        match document_symbols_on_connection(&connection, &scoped, limit).await {
            Ok((found, capped)) => {
                truncated |= capped;
                symbols.extend(found);
                attempts.push(LspAttempt::success(
                    *language,
                    &info,
                    "textDocument/documentSymbol",
                ));
            }
            Err(error) => {
                let error = attempt_error_from_lsp(*language, &info, error);
                attempts.push(LspAttempt::failure(*language, &error));
                errors.push(error);
            }
        }
    }
    if attempts.iter().all(|attempt| !attempt.status.is_success()) {
        return Err(LspAttemptError::merge(errors));
    }
    let (symbols, capped) = prepare_symbols(symbols, None, limit);
    Ok(FileSymbolBatch {
        symbols,
        status: if attempts.iter().any(|attempt| !attempt.status.is_success())
            || capped
            || truncated
        {
            Status::Partial
        } else {
            Status::Ok
        },
        attempts,
    })
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LspStatusEntry {
    pub language: String,
    pub display: String,
    pub server: Option<String>,
    pub installed: bool,
    pub running: bool,
    pub capabilities: Vec<String>,
    pub install_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LspStatusReport {
    pub root: String,
    pub languages: Vec<LspStatusEntry>,
    pub running: usize,
    pub installed: usize,
}

/// Non-spawning status used by the CLI/daemon. It only checks `PATH` and the
/// in-process registry; it never starts a server.
pub(crate) fn status_report(root: &Path) -> LspStatusReport {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let now = Instant::now();
    let servers = registry()
        .lock()
        .map(|servers| {
            servers
                .iter()
                .filter(|(key, _)| key.root == canonical)
                .map(|(key, server)| (key.language.clone(), server.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut languages = Vec::new();
    let mut running = 0usize;
    let mut installed = 0usize;
    for language in LANGUAGES.iter().take(MAX_STATUS_LANGUAGES) {
        let server = servers.get(language.id);
        let alive = server.is_some_and(|server| server.alive.load(Ordering::SeqCst));
        if alive {
            running += 1;
        }
        let candidate = discover_server(*language);
        let available = alive || candidate.is_some();
        if available {
            installed += 1;
        }
        let (capabilities, name, idle_secs) = match server {
            Some(server) if alive => (
                server
                    .capabilities
                    .names()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                server
                    .server_info
                    .as_ref()
                    .map(|info| info.name.clone())
                    .or_else(|| candidate.as_ref().map(|(program, _)| program.clone())),
                server
                    .last_used
                    .lock()
                    .ok()
                    .map(|last| now.duration_since(*last).as_secs()),
            ),
            _ => (
                Vec::new(),
                candidate.as_ref().map(|(program, _)| program.clone()),
                None,
            ),
        };
        languages.push(LspStatusEntry {
            language: language.id.to_string(),
            display: language.display.to_string(),
            server: name,
            installed: available,
            running: alive,
            capabilities,
            install_hint: language.install_hint.to_string(),
            idle_secs,
        });
    }
    LspStatusReport {
        root: canonical.to_string_lossy().to_string(),
        languages,
        running,
        installed,
    }
}

fn unavailable_output(error: &LspAttemptError, action: &str) -> ToolOutput {
    let status = error.status();
    let mut structured = json!({
        "action": action,
        "code": error.code(),
        "status": status.as_str(),
        "source": "lsp",
        "confidence": "lsp",
        "confidence_score": 0.0,
        "message": error.to_string(),
        "fallback_reason": error.reason(),
        "searched": error.searched(),
        "supported_languages": all_languages().iter().map(|language| language.id).collect::<Vec<_>>(),
    });
    if let Some(language) = error.language() {
        structured["language"] = json!(language);
    }
    if let Some(server) = error.server() {
        structured["language_server"] = json!(server);
    }
    if let Some(hint) = error.install_hint() {
        structured["install_hint"] = json!(hint);
    }
    if let Some(capabilities) = error.capabilities() {
        structured["capabilities"] = capabilities.summary();
    }
    ToolOutput::error_structured(error.to_string(), structured)
}

#[async_trait]
impl AgentTool for LspQuery {
    fn name(&self) -> &'static str {
        "lsp_query"
    }

    fn description(&self) -> &'static str {
        "Query an installed language server for definitions, references, hover, document/workspace symbols, diagnostics, capabilities, or status. Never installs a server; missing servers return LSP_UNAVAILABLE with an install hint."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["definition", "references", "hover", "symbols", "workspace_symbols", "diagnostics", "capabilities", "status"]},
                "path": {"type": "string", "description": "File path relative to the worktree root"},
                "language": {"type": "string", "description": "Optional language id for workspace_symbols or capabilities"},
                "line": {"type": "integer", "minimum": 1, "description": "1-based line"},
                "column": {"type": "integer", "minimum": 1, "description": "1-based character column"},
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
        if action == "status" {
            let report = status_report(&context.root);
            let mut structured = json!({
                "action": "status",
                "root": report.root,
                "languages": report.languages,
                "running": report.running,
                "installed": report.installed,
                "supported_languages": all_languages().iter().map(|language| language.id).collect::<Vec<_>>(),
            });
            Backend::lsp("unknown", None, "status", Status::Ok)
                .with_score(1.0)
                .apply(&mut structured);
            return Ok(ToolOutput::ok_structured(
                format!(
                    "{} language servers running · {} installed",
                    report.running, report.installed
                ),
                structured,
            ));
        }
        let path = input.get("path").and_then(Value::as_str);
        let language_hint = input.get("language").and_then(Value::as_str);
        if !matches!(action, "workspace_symbols" | "capabilities" | "status") && path.is_none() {
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
        } else if let Some(language) = language_hint {
            language_by_name(language)
        } else {
            first_source_file(&context.root).and_then(|path| language_for_path(&path))
        };
        let Some(language) = language else {
            let error = LspAttemptError::NoLanguage {
                supported: all_languages()
                    .iter()
                    .map(|language| language.id.to_string())
                    .collect(),
            };
            return Ok(unavailable_output(&error, action));
        };
        let connection = match connect(context, language).await {
            Ok(connection) => connection,
            Err(error) => {
                let error = attempt_error_from_connect(language, error);
                return Ok(unavailable_output(&error, action));
            }
        };
        let info = connection.info();
        if action == "capabilities" {
            let mut structured = json!({
                "action": "capabilities",
                "language": info.language,
                "language_server": info.server,
                "language_server_version": info.server_version,
                "capabilities": info.capabilities.summary(),
                "methods": info.capabilities.names(),
            });
            Backend::lsp(
                &info.language,
                info.server.as_deref(),
                "initialize",
                Status::Ok,
            )
            .with_score(1.0)
            .apply(&mut structured);
            return Ok(ToolOutput::ok_structured(
                format!(
                    "{} · {} · {}",
                    info.server.as_deref().unwrap_or("language server"),
                    info.language,
                    info.capabilities.names().join(", ")
                ),
                structured,
            ));
        }
        let result: Result<(Value, String, Status, String), LspAttemptError> = match action {
            "definition" | "references" | "hover" => {
                let method = match action {
                    "definition" => "textDocument/definition",
                    "references" => "textDocument/references",
                    "hover" => "textDocument/hover",
                    _ => unreachable!(),
                };
                if !info.capabilities.supports(method) {
                    return Ok(unavailable_output(
                        &unsupported(language, &info, method),
                        action,
                    ));
                }
                let file = file.as_deref().expect("checked above");
                let line = input
                    .get("line")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("line is required"))?;
                let column = input
                    .get("column")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("column is required"))?;
                let (line_zero, character) =
                    position_for(file, line, column, info.capabilities.position_encoding)?;
                let document = connection
                    .open_document(file)
                    .await
                    .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                let position = json!({"line": line_zero, "character": character});
                if action == "definition" {
                    let value = connection
                        .request(
                            "textDocument/definition",
                            json!({"textDocument": {"uri": document.uri}, "position": position}),
                        )
                        .await
                        .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                    let root = connection.root();
                    let mut cache =
                        SourceCache::with_encoding(&root, info.capabilities.position_encoding);
                    let locations = locations_from_value(&mut cache, &root, &value, limit.max(1));
                    let (locations, truncated) = prepare_locations(locations, limit);
                    let rendered = render_locations(&locations);
                    Ok((
                        json!({"locations": locations, "truncated": truncated}),
                        rendered,
                        Status::Ok,
                        method.to_string(),
                    ))
                } else if action == "references" {
                    let include_declaration = input
                        .get("include_declaration")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                    let value = connection
                        .request(
                            "textDocument/references",
                            json!({
                                "textDocument": {"uri": document.uri},
                                "position": position,
                                "context": {"includeDeclaration": include_declaration},
                            }),
                        )
                        .await
                        .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                    let root = connection.root();
                    let mut cache =
                        SourceCache::with_encoding(&root, info.capabilities.position_encoding);
                    let locations = locations_from_value(&mut cache, &root, &value, limit.max(1));
                    let (locations, truncated) = prepare_locations(locations, limit);
                    let rendered = render_locations(&locations);
                    Ok((
                        json!({"locations": locations, "truncated": truncated}),
                        rendered,
                        Status::Ok,
                        method.to_string(),
                    ))
                } else {
                    let value = connection
                        .request(
                            "textDocument/hover",
                            json!({"textDocument": {"uri": document.uri}, "position": position}),
                        )
                        .await
                        .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                    let text = value.get("contents").map(hover_text).unwrap_or_default();
                    let root = connection.root();
                    let mut cache =
                        SourceCache::with_encoding(&root, info.capabilities.position_encoding);
                    let range = value
                        .get("range")
                        .and_then(|range| cache.map_range(file, range))
                        .map(|(range, _)| range);
                    Ok((
                        json!({"hover": text, "range": range}),
                        text,
                        Status::Ok,
                        method.to_string(),
                    ))
                }
            }
            "symbols" => {
                if !info.capabilities.document_symbols {
                    return Ok(unavailable_output(
                        &unsupported(language, &info, "textDocument/documentSymbol"),
                        action,
                    ));
                }
                let file = file.as_deref().expect("checked above");
                let document = connection
                    .open_document(file)
                    .await
                    .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                let value = connection
                    .request(
                        "textDocument/documentSymbol",
                        json!({"textDocument": {"uri": document.uri}}),
                    )
                    .await
                    .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                let root = connection.root();
                let mut cache =
                    SourceCache::with_encoding(&root, info.capabilities.position_encoding);
                let symbols = document_symbols_from_value(
                    &value,
                    &mut cache,
                    &root,
                    file,
                    &info.display_language,
                    limit.max(1),
                );
                let (symbols, truncated) = prepare_symbols(symbols, None, limit);
                let rendered = render_symbols(&symbols);
                Ok((
                    json!({"symbols": symbols, "truncated": truncated}),
                    rendered,
                    Status::Ok,
                    "textDocument/documentSymbol".to_string(),
                ))
            }
            "workspace_symbols" => {
                if !info.capabilities.workspace_symbols {
                    return Ok(unavailable_output(
                        &unsupported(language, &info, "workspace/symbol"),
                        action,
                    ));
                }
                let query = input.get("query").and_then(Value::as_str).unwrap_or("");
                let value = connection
                    .request("workspace/symbol", json!({"query": query}))
                    .await
                    .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                let root = connection.root();
                let mut cache =
                    SourceCache::with_encoding(&root, info.capabilities.position_encoding);
                let mut symbols = workspace_symbols_from_value(
                    &value,
                    &mut cache,
                    &root,
                    &info.display_language,
                    MAX_SYMBOLS,
                );
                for symbol in &mut symbols {
                    symbol.score = score_name_match(&symbol.name, query);
                }
                if !query.trim().is_empty() {
                    symbols.retain(|symbol| symbol.score > 0.0);
                }
                let (symbols, truncated) = prepare_symbols(symbols, Some(query), limit);
                let rendered = render_symbols(&symbols);
                Ok((
                    json!({"query": query, "symbols": symbols, "truncated": truncated}),
                    rendered,
                    Status::Ok,
                    "workspace/symbol".to_string(),
                ))
            }
            "diagnostics" => {
                let file = file.as_deref().expect("checked above");
                let scan = diagnostics_on_connection(&connection, &[file.to_path_buf()], limit)
                    .await
                    .map_err(|error| attempt_error_from_lsp(language, &info, error))?;
                if scan.status == Status::Timeout {
                    return Ok(unavailable_output(
                        &LspAttemptError::Failed {
                            language: language.id.to_string(),
                            display: language.display.to_string(),
                            server: info.server.clone(),
                            error: Box::new(LspError::Timeout {
                                method: scan.method,
                                secs: LSP_DIAGNOSTIC_WAIT_MS / 1_000,
                            }),
                        },
                        action,
                    ));
                }
                let summary = prepare_diagnostics(scan.diagnostics, limit);
                let rendered = render_diagnostics(&summary);
                Ok((
                    json!({
                        "diagnostics": summary.groups.iter().flat_map(|group| group.diagnostics.clone()).collect::<Vec<_>>(),
                        "groups": summary.groups,
                        "error_count": summary.errors,
                        "warning_count": summary.warnings,
                        "ok": summary.ok(),
                        "method": scan.method,
                        "truncated": scan.truncated || summary.truncated,
                    }),
                    rendered,
                    scan.status,
                    scan.method,
                ))
            }
            other => {
                return Ok(ToolOutput::error_structured(
                    format!("unsupported LSP action: {other}"),
                    json!({"code": "INVALID_ARGUMENT", "action": other}),
                ));
            }
        };
        let (mut structured, rendered, status, method) = match result {
            Ok(value) => value,
            Err(error) => return Ok(unavailable_output(&error, action)),
        };
        let backend = Backend::lsp(&info.language, info.server.as_deref(), &method, status)
            .with_language(&info.display_language)
            .with_score(match status {
                Status::Ok => LSP_CONFIDENCE_DEFINITION,
                _ => LSP_CONFIDENCE_SYMBOL,
            });
        backend.apply(&mut structured);
        let header = backend_sentence(&backend);
        let text = if rendered.is_empty() {
            format!("{header}\nlanguage server returned no result")
        } else {
            format!("{header}\n{rendered}")
        };
        Ok(ToolOutput::ok_structured(truncate(text), structured))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
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

    fn write_fake_server(root: &Path) -> PathBuf {
        let path = root.join("fake_lsp.py");
        std::fs::write(&path, FAKE_SERVER).unwrap();
        path
    }

    async fn fake_connection(root: &Path, mode: &str) -> (LspConnection, ServerKey, PathBuf) {
        write_fake_server(root);
        let context = context(root);
        let root = root.canonicalize().unwrap();
        let key = ServerKey {
            root: root.clone(),
            language: "rust".into(),
        };
        let language = language_for_path(Path::new("a.rs")).unwrap();
        let server = start_server_with(&context, &key, language, "python3", &["fake_lsp.py", mode])
            .await
            .unwrap();
        registry()
            .lock()
            .unwrap()
            .insert(key.clone(), server.clone());
        (LspConnection { server }, key, root.join("src/lib.rs"))
    }

    const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
mode = sys.argv[1] if len(sys.argv) > 1 else "full"
marker = sys.argv[2] if len(sys.argv) > 2 else ""
root_uri = None

def read_message():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(value):
    body = json.dumps(value).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()

def error(message, ident, code=-32601):
    send({"jsonrpc": "2.0", "id": ident, "error": {"code": code, "message": message}})

def uri_for(name):
    return (root_uri or "") + "/" + name

def range_obj(line, char, end_line, end_char):
    return {"start": {"line": line, "character": char}, "end": {"line": end_line, "character": end_char}}

opened = {}
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    params = message.get("params") or {}
    ident = message.get("id")
    if method == "initialize":
        root_uri = params.get("rootUri") or (params.get("workspaceFolders") or [{}])[0].get("uri")
        caps = {
            "definitionProvider": True,
            "referencesProvider": mode != "definition_only",
            "hoverProvider": True,
            "documentSymbolProvider": True,
            "workspaceSymbolProvider": mode not in ("no_workspace", "unsupported"),
            "diagnosticProvider": {"interFileDependencies": True, "workspaceDiagnostics": False} if mode in ("full", "diagnostics") else False,
            "positionEncoding": "utf-8" if mode == "utf8" else "utf-16",
        }
        send({"jsonrpc": "2.0", "id": ident, "result": {"capabilities": caps, "serverInfo": {"name": "fake-lsp", "version": "1.2.3"}}})
    elif method == "textDocument/didOpen":
        doc = params["textDocument"]
        opened[doc["uri"]] = doc.get("version", 1)
        if mode == "publish":
            send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {
                "uri": doc["uri"], "version": doc.get("version", 1),
                "diagnostics": [
                    {"range": range_obj(1, 0, 1, 4), "severity": 1, "message": "published error", "source": "fake", "code": "P1"},
                    {"range": range_obj(1, 0, 1, 4), "severity": 1, "message": "published error", "source": "fake", "code": "P1"},
                ]}})
    elif method == "textDocument/didChange":
        doc = params["textDocument"]
        opened[doc["uri"]] = doc.get("version", 1)
    elif method == "workspace/symbol":
        if mode in ("no_workspace", "unsupported", "workspace_error"):
            error("workspace/symbol not supported", ident)
        elif mode == "timeout":
            time.sleep(30)
        elif mode == "crash":
            sys.exit(3)
        elif mode == "crash_once":
            if marker and not os.path.exists(marker):
                open(marker, "w").write("crashed")
                sys.exit(4)
            send({"jsonrpc": "2.0", "id": ident, "result": [
                {"name": "widget", "kind": 12, "location": {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)}},
                {"name": "widget_builder", "kind": 12, "location": {"uri": uri_for("src/lib.rs"), "range": range_obj(2, 0, 2, 10)}},
            ]})
        elif mode == "utf8":
            send({"jsonrpc": "2.0", "id": ident, "result": [
                {"name": "widget", "kind": 12, "location": {"uri": uri_for("src/unicode.rs"), "range": range_obj(0, 11, 0, 17)}},
            ]})
        else:
            send({"jsonrpc": "2.0", "id": ident, "result": [
                {"name": "widget", "kind": 12, "containerName": "demo", "location": {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)}},
                {"name": "widget", "kind": 12, "containerName": "demo", "location": {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)}},
                {"name": "outside", "kind": 12, "location": {"uri": "file:///etc/passwd", "range": range_obj(0, 0, 0, 1)}},
                {"name": "widget_builder", "kind": 12, "location": {"uri": uri_for("src/lib.rs"), "range": range_obj(2, 0, 2, 10)}},
            ] + [
                {"name": f"widget_{i}", "kind": 12, "location": {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)}} for i in range(120)
            ]})
    elif method == "textDocument/documentSymbol":
        if mode == "unsupported":
            error("documentSymbol not supported", ident)
        else:
            send({"jsonrpc": "2.0", "id": ident, "result": [
                {"name": "widget", "kind": 12, "range": range_obj(1, 0, 3, 1), "selectionRange": range_obj(1, 3, 1, 9), "children": [
                    {"name": "helper", "kind": 6, "range": range_obj(2, 2, 2, 20), "selectionRange": range_obj(2, 6, 2, 12)}
                ]},
                {"name": "Widget", "kind": 23, "range": range_obj(4, 0, 4, 13), "selectionRange": range_obj(4, 7, 4, 13)}
            ]})
    elif method == "textDocument/definition":
        send({"jsonrpc": "2.0", "id": ident, "result": {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)}})
    elif method == "textDocument/references":
        if mode == "definition_only":
            error("references not supported", ident)
        else:
            send({"jsonrpc": "2.0", "id": ident, "result": [
                {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)},
                {"uri": uri_for("src/lib.rs"), "range": range_obj(1, 3, 1, 9)},
                {"uri": uri_for("src/other.rs"), "range": range_obj(0, 0, 0, 6)},
                {"uri": "file:///etc/passwd", "range": range_obj(0, 0, 0, 1)},
            ]})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": ident, "result": {"contents": {"kind": "markdown", "value": "**fn widget()**"}, "range": range_obj(1, 3, 1, 9)}})
    elif method == "textDocument/diagnostic":
        if mode in ("full", "diagnostics"):
            send({"jsonrpc": "2.0", "id": ident, "result": {"kind": "full", "items": [
                {"range": range_obj(1, 0, 1, 4), "severity": 1, "message": "pull error", "source": "fake", "code": "E1"},
                {"range": range_obj(1, 0, 1, 4), "severity": 1, "message": "pull error", "source": "fake", "code": "E1"},
                {"range": range_obj(2, 0, 2, 4), "severity": 2, "message": "pull warning", "source": "fake", "code": "W1"},
            ]}})
        else:
            error("textDocument/diagnostic not supported", ident)
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": ident, "result": None})
    elif method == "exit":
        break
    elif ident is not None:
        error("method not found", ident)
"#;

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "// header\nfn widget() {}\nfn widget_builder() {}\n\nstruct Widget;\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/other.rs"), "fn widget() {}\n").unwrap();
        std::fs::write(dir.path().join("src/unicode.rs"), "// 😀 fn widget() {}\n").unwrap();
        dir
    }

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
        assert_eq!(
            language_for_path(Path::new("src/app.mjs")).map(|language| language.id),
            Some("javascript")
        );
        assert_eq!(
            language_for_path(Path::new("src/App.swift")).map(|language| language.id),
            Some("swift")
        );
        assert!(language_for_path(Path::new("README.md")).is_none());
        assert_eq!(
            language_by_name("Rust").map(|language| language.id),
            Some("rust")
        );
        assert_eq!(
            language_by_name("cpp").map(|language| language.id),
            Some("cpp")
        );
        assert!(find_on_path("definitely-not-a-real-language-server").is_none());
    }

    #[test]
    fn orders_languages_for_a_polyglot_file_set() {
        let files = vec![
            PathBuf::from("src/a.rs"),
            PathBuf::from("src/b.rs"),
            PathBuf::from("web/app.ts"),
            PathBuf::from("README.md"),
        ];
        let languages = languages_for_files(&files, 2);
        assert_eq!(languages.len(), 2);
        assert_eq!(languages[0].id, "rust");
        assert_eq!(languages[1].id, "typescript");
        let languages = languages_for_files(&files, 1);
        assert_eq!(languages.len(), 1);
        assert_eq!(languages[0].id, "rust");
    }

    #[test]
    fn maps_utf16_ranges_back_to_character_columns() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("emoji.rs");
        std::fs::write(&path, "a😀b\nnaïve😀x\n").unwrap();
        let mut cache = SourceCache::new(dir.path());
        let value = json!({
            "start": {"line": 0, "character": 4},
            "end": {"line": 1, "character": 7}
        });
        let (range, context) = cache.map_range(&path, &value).unwrap();
        assert_eq!(range.line, 1);
        assert_eq!(range.column, 4);
        assert_eq!(range.end_line, 2);
        assert_eq!(range.end_column, 7);
        assert_eq!(context, "a😀b");
        assert_eq!(character_column("a😀b", 2, PositionEncoding::Utf16), 3);
        assert_eq!(character_column("a😀b", 3, PositionEncoding::Utf16), 3);
        assert_eq!(character_column("a😀b", 1, PositionEncoding::Utf16), 2);
    }

    #[test]
    fn supports_negotiated_utf8_and_utf32_position_encodings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "a😀b\n").unwrap();
        assert_eq!(
            position_for(&path, 1, 3, PositionEncoding::Utf8).unwrap(),
            (0, 5)
        );
        assert_eq!(
            position_for(&path, 1, 3, PositionEncoding::Utf32).unwrap(),
            (0, 2)
        );
        assert_eq!(character_column("a😀b", 5, PositionEncoding::Utf8), 3);
        assert_eq!(character_column("a😀b", 2, PositionEncoding::Utf32), 3);

        let value = json!({
            "start": {"line": 0, "character": 5},
            "end": {"line": 0, "character": 5}
        });
        let mut utf8 = SourceCache::with_encoding(dir.path(), PositionEncoding::Utf8);
        let (range, _) = utf8.map_range(&path, &value).unwrap();
        assert_eq!(range.column, 3);
        let mut utf32 = SourceCache::with_encoding(dir.path(), PositionEncoding::Utf32);
        let value = json!({
            "start": {"line": 0, "character": 2},
            "end": {"line": 0, "character": 2}
        });
        let (range, _) = utf32.map_range(&path, &value).unwrap();
        assert_eq!(range.column, 3);
    }

    #[test]
    fn capability_parser_reads_position_encoding() {
        let utf8 = LspCapabilities::from_initialize(&json!({
            "capabilities": {"positionEncoding": "utf-8"}
        }));
        assert_eq!(utf8.position_encoding, PositionEncoding::Utf8);
        let utf32 = LspCapabilities::from_initialize(&json!({
            "capabilities": {"positionEncoding": "utf-32"}
        }));
        assert_eq!(utf32.position_encoding, PositionEncoding::Utf32);
        let default = LspCapabilities::from_initialize(&json!({"capabilities": {}}));
        assert_eq!(default.position_encoding, PositionEncoding::Utf16);
    }

    #[test]
    fn converts_positions_to_utf16_units() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "a😀b\n").unwrap();
        let (line, character) = position_for(&path, 1, 3, PositionEncoding::Utf16).unwrap();
        assert_eq!(line, 0);
        assert_eq!(character, 3);
    }

    #[test]
    fn rejects_uris_outside_the_workspace() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn x() {}\n").unwrap();
        std::fs::write(dir.path().join("outside.rs"), "fn y() {}\n").unwrap();
        let inside = reqwest::Url::from_file_path(root.join("src/lib.rs"))
            .unwrap()
            .to_string();
        assert_eq!(
            uri_to_path(&root, &inside).unwrap(),
            root.join("src/lib.rs").canonicalize().unwrap()
        );
        let outside = reqwest::Url::from_file_path(dir.path().join("outside.rs"))
            .unwrap()
            .to_string();
        assert!(uri_to_path(&root, &outside).is_none());
        assert!(uri_to_path(&root, "https://example.com/x.rs").is_none());
        assert!(uri_to_path(&root, "file:///etc/passwd").is_none());
        let traversal = format!("file://{}/%2e%2e/outside.rs", root.to_string_lossy());
        assert!(uri_to_path(&root, &traversal).is_none());
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

    #[tokio::test]
    async fn rejects_framing_without_length_and_oversized_bodies() {
        let bytes = b"X-Test: yes\r\n\r\n{}".to_vec();
        let mut reader = BufReader::new(bytes.as_slice());
        let error = read_message(&mut reader).await.unwrap_err();
        assert!(error.to_string().contains("Content-Length"));

        let frame = format!("Content-Length: {}\r\n\r\n", MAX_LSP_MESSAGE_BYTES + 1);
        let mut reader = BufReader::new(frame.as_bytes());
        let error = read_message(&mut reader).await.unwrap_err();
        assert!(error.to_string().contains("oversized"));
    }

    #[test]
    fn parses_capabilities_in_all_shapes() {
        let value = json!({"capabilities": {
            "definitionProvider": true,
            "referencesProvider": {"workDoneProgress": false},
            "hoverProvider": false,
            "documentSymbolProvider": {},
            "workspaceSymbolProvider": true,
            "diagnosticProvider": {"interFileDependencies": true}
        }});
        let capabilities = LspCapabilities::from_initialize(&value);
        assert!(capabilities.definition);
        assert!(capabilities.references);
        assert!(!capabilities.hover);
        assert!(capabilities.document_symbols);
        assert!(capabilities.workspace_symbols);
        assert!(capabilities.pull_diagnostics);
        assert!(capabilities.publish_diagnostics);
        assert!(capabilities.supports("workspace/symbol"));
        assert!(!capabilities.supports("textDocument/hover"));
        assert!(capabilities.names().contains(&"references"));
        assert_eq!(capabilities.summary()["definition"], true);
    }

    #[test]
    fn maps_symbols_and_diagnostics_into_the_shared_contract() {
        let dir = fixture();
        let mut cache = SourceCache::new(dir.path());
        let value = json!([{
            "name": "Widget",
            "kind": 23,
            "range": {"start": {"line": 4, "character": 0}, "end": {"line": 4, "character": 13}},
            "selectionRange": {"start": {"line": 4, "character": 7}, "end": {"line": 4, "character": 13}},
            "children": [{
                "name": "new",
                "kind": 6,
                "range": {"start": {"line": 0, "character": 2}, "end": {"line": 0, "character": 10}},
                "selectionRange": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 9}}
            }]
        }]);
        let file = dir.path().join("src/lib.rs");
        let symbols =
            document_symbols_from_value(&value, &mut cache, dir.path(), &file, "Rust", 10);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].kind, "struct");
        assert_eq!(symbols[0].kind_group, "type");
        assert_eq!(symbols[0].line, 5);
        assert_eq!(symbols[0].column, 8);
        assert_eq!(symbols[1].container, Some("Widget".to_string()));
        assert_eq!(symbols[1].source, "lsp");

        let diagnostic = json!({
            "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 4}},
            "severity": 1,
            "message": "cannot find value",
            "source": "rust-analyzer",
            "code": "E0425"
        });
        let diagnostics = diagnostics_from_items(
            &[diagnostic],
            &mut cache,
            dir.path(),
            Some(&file),
            Some("rust-analyzer"),
            10,
        );
        assert_eq!(diagnostics[0].line, 2);
        assert_eq!(diagnostics[0].severity, "error");
        assert_eq!(diagnostics[0].server.as_deref(), Some("rust-analyzer"));
        assert_eq!(diagnostics[0].code.as_ref().unwrap(), &json!("E0425"));
    }

    #[test]
    fn workspace_symbols_dedupe_jail_and_cap() {
        let dir = fixture();
        let root = dir.path();
        let uri = reqwest::Url::from_file_path(root.join("src/lib.rs"))
            .unwrap()
            .to_string();
        let value = json!([
            {"name": "widget", "kind": 12, "location": {"uri": uri, "range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 9}}}},
            {"name": "widget", "kind": 12, "location": {"uri": uri, "range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 9}}}},
            {"name": "escape", "kind": 12, "location": {"uri": "file:///etc/passwd", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}},
            {"name": "widget_builder", "kind": 12, "location": {"uri": uri, "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 10}}}}
        ]);
        let mut cache = SourceCache::new(root);
        let symbols = workspace_symbols_from_value(&value, &mut cache, root, "Rust", 10);
        let (symbols, truncated) = prepare_symbols(symbols, Some("widget"), 10);
        assert!(!truncated);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].name, "widget");
        assert_eq!(symbols[0].column, 4);
        assert_eq!(symbols[1].name, "widget_builder");
    }

    #[tokio::test]
    async fn fake_server_reports_capabilities_and_server_info() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let info = connection.info();
        assert_eq!(info.language, "rust");
        assert!(info.capabilities.workspace_symbols);
        assert!(info.capabilities.definition);
        assert!(info.capabilities.pull_diagnostics);
        assert_eq!(info.server.as_deref(), Some("fake-lsp"));
        assert_eq!(info.server_version.as_deref(), Some("1.2.3"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn workspace_symbols_are_bounded_deduped_and_jailed_end_to_end() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let scope = vec![dir.path().join("src/lib.rs")];
        let scan = symbols_on_connection(&connection, "widget", &scope, 8)
            .await
            .unwrap();
        assert_eq!(scan.method, "workspace/symbol");
        assert_eq!(scan.symbols.len(), 8);
        assert_eq!(scan.symbols[0].name, "widget");
        assert_eq!(scan.symbols.len(), 8);
        assert!(
            scan.symbols
                .iter()
                .all(|symbol| !symbol.path.contains("passwd"))
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn fake_server_definition_references_and_hover_map_ranges() {
        let dir = fixture();
        let (connection, key, file) = fake_connection(dir.path(), "full").await;
        let definition = connection
            .request(
                "textDocument/definition",
                json!({"textDocument": {"uri": connection.open_or_update(&file).await.unwrap()}, "position": {"line": 1, "character": 4}}),
            )
            .await
            .unwrap();
        let root = connection.root();
        let mut cache = SourceCache::new(&root);
        let locations = locations_from_value(&mut cache, &root, &definition, 10);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].line, 2);
        assert_eq!(locations[0].column, 4);
        assert_eq!(locations[0].context.as_deref(), Some("fn widget() {}"));

        let scan = references_on_connection(
            &connection,
            &DeclarationPosition {
                path: file.clone(),
                line: 2,
                column: 4,
                kind: Some("function".into()),
            },
            true,
            10,
        )
        .await
        .unwrap();
        assert_eq!(scan.method, "textDocument/references");
        assert_eq!(scan.locations.len(), 2);
        assert_eq!(scan.locations[0].path, "src/lib.rs");
        assert!(
            scan.locations
                .iter()
                .all(|location| !location.path.contains("passwd"))
        );
        assert!(scan.locations[0].is_definition);
        assert_eq!(scan.locations[0].kind.as_deref(), Some("function"));

        let hover = connection.hover(&file, 2, 4).await.unwrap();
        assert!(hover_text(&hover["contents"]).contains("widget"));
        let range = hover.get("range").unwrap();
        let (mapped, _) = SourceCache::new(&root).map_range(&file, range).unwrap();
        assert_eq!(mapped.line, 2);
        assert_eq!(mapped.column, 4);
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn pull_diagnostics_are_grouped_and_deduped() {
        let dir = fixture();
        let (connection, key, file) = fake_connection(dir.path(), "diagnostics").await;
        let scan = diagnostics_on_connection(&connection, &[file], 100)
            .await
            .unwrap();
        assert_eq!(scan.method, "textDocument/diagnostic");
        assert_eq!(scan.diagnostics.len(), 2);
        let summary = prepare_diagnostics(scan.diagnostics, 100);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 1);
        assert_eq!(summary.groups.len(), 1);
        assert_eq!(summary.groups[0].path, "src/lib.rs");
        assert!(!summary.ok());
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn publish_diagnostics_are_accepted_with_the_document_version() {
        let dir = fixture();
        let (connection, key, file) = fake_connection(dir.path(), "publish").await;
        let scan = diagnostics_on_connection(&connection, &[file], 100)
            .await
            .unwrap();
        assert_eq!(scan.method, "textDocument/publishDiagnostics");
        assert_eq!(scan.diagnostics.len(), 1);
        assert_eq!(scan.diagnostics[0].message, "published error");
        assert_eq!(scan.diagnostics[0].server.as_deref(), Some("fake-lsp"));
        assert!(scan.status.is_success());
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn unsupported_method_is_typed_not_a_string_guess() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "unsupported").await;
        let error = symbols_on_connection(&connection, "widget", &[], 5)
            .await
            .unwrap_err();
        assert!(error.is_method_not_found());
        assert_eq!(error.code(), "LSP_METHOD_UNAVAILABLE");
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn request_timeout_is_reported_with_method_and_bound() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "timeout").await;
        let error = connection
            .request_with_timeout(
                "workspace/symbol",
                json!({"query": "widget"}),
                Duration::from_millis(200),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, LspError::Timeout { .. }));
        assert_eq!(error.status(), Status::Timeout);
        assert!(error.to_string().contains("workspace/symbol"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn crash_restart_reconnects_once_and_succeeds() {
        let dir = fixture();
        let marker = dir.path().join("crash.marker");
        write_fake_server(dir.path());
        let context = context(dir.path());
        let root = dir.path().canonicalize().unwrap();
        let key = ServerKey {
            root: root.clone(),
            language: "rust".into(),
        };
        let language = language_for_path(Path::new("a.rs")).unwrap();
        let server = start_server_with(
            &context,
            &key,
            language,
            "python3",
            &["fake_lsp.py", "crash_once", marker.to_str().unwrap()],
        )
        .await
        .unwrap();
        registry().lock().unwrap().insert(key.clone(), server);
        let scope = vec![dir.path().join("src/lib.rs")];
        let scan = match search_symbols_in_language(&context, language, "widget", &scope, 5, true)
            .await
        {
            Ok(scan) => scan,
            Err(error) => {
                let keys = registry()
                    .lock()
                    .map(|servers| servers.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let alive = registry().lock().ok().and_then(|servers| {
                    servers
                        .get(&key)
                        .map(|server| server.alive.load(Ordering::SeqCst))
                });
                panic!(
                    "search failed: {error}; key={key:?}; registry={keys:?}; alive={alive:?}; marker={}",
                    marker.exists()
                );
            }
        };
        assert_eq!(scan.0.len(), 2);
        let replaced = registry().lock().unwrap().get(&key).cloned();
        assert!(
            replaced
                .as_ref()
                .is_some_and(|server| server.alive.load(Ordering::SeqCst))
        );
        if let Some(server) = replaced {
            kill_server(&server);
        }
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn missing_server_reports_install_hint() {
        let dir = fixture();
        let context = context(dir.path());
        static MISSING: LspLanguage = LspLanguage {
            id: "missing",
            display: "Missing",
            extensions: &["missing"],
            candidates: &[("definitely-not-a-real-language-server", &[])],
            install_hint: "install a fake language server",
        };
        let error = match connect(&context, MISSING).await {
            Ok(_) => panic!("expected a missing language server error"),
            Err(error) => error,
        };
        assert!(matches!(error, LspConnectError::NotInstalled { .. }));
        assert_eq!(error.code(), "LSP_UNAVAILABLE");
        assert!(
            error
                .install_hint()
                .unwrap()
                .contains("install a fake language server")
        );
        assert!(
            error
                .searched()
                .iter()
                .any(|program| program == "definitely-not-a-real-language-server")
        );
    }

    #[tokio::test]
    async fn document_symbol_fallback_when_workspace_symbols_are_absent() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "no_workspace").await;
        assert!(!connection.capabilities().workspace_symbols);
        assert!(connection.capabilities().document_symbols);
        let scope = vec![dir.path().join("src/lib.rs")];
        let scan = symbols_on_connection(&connection, "widget", &scope, 10)
            .await
            .unwrap();
        assert_eq!(scan.method, "textDocument/documentSymbol");
        assert_eq!(scan.status, Status::Partial);
        assert!(scan.symbols.iter().any(|symbol| symbol.name == "widget"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn document_size_cap_refuses_oversized_documents() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            vec![b'a'; MAX_LSP_DOCUMENT_BYTES as usize + 1],
        )
        .unwrap();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let error = connection
            .open_document(&dir.path().join("src/lib.rs"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("larger than"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn utf8_position_encoding_is_negotiated_and_mapped_to_character_columns() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "utf8").await;
        assert_eq!(
            connection.capabilities().position_encoding,
            PositionEncoding::Utf8
        );
        let scope = vec![dir.path().join("src/unicode.rs")];
        let scan = symbols_on_connection(&connection, "widget", &scope, 10)
            .await
            .unwrap();
        assert_eq!(scan.symbols.len(), 1);
        assert_eq!(scan.symbols[0].path, "src/unicode.rs");
        assert_eq!(scan.symbols[0].line, 1);
        assert_eq!(scan.symbols[0].column, 9);
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn high_level_find_symbol_uses_the_fake_server_and_labels_exact() {
        use crate::tooling::code::FindSymbol;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let output = FindSymbol
            .execute(
                &context(dir.path()),
                json!({"query": "widget", "source": "lsp", "language": "Rust", "limit": 5}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["status"], "ok", "structured: {structured}");
        assert_eq!(structured["language_server"], "fake-lsp");
        assert_eq!(structured["method"], "workspace/symbol");
        assert_eq!(structured["symbols"][0]["path"], "src/lib.rs");
        assert_eq!(structured["symbols"][0]["line"], 2);
        assert_eq!(structured["symbols"][0]["column"], 4);
        assert_eq!(structured["symbols"][0]["range"]["line"], 2);
        assert_eq!(structured["symbols"][0]["kind"], "function");
        assert_eq!(structured["symbols"][0]["source"], "lsp");
        assert!(output.content.contains("LSP · fake-lsp"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn high_level_find_references_uses_the_fake_server_and_definition_marker() {
        use crate::tooling::code::FindReferences;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let output = FindReferences
            .execute(
                &context(dir.path()),
                json!({
                    "name": "widget",
                    "path": "src/lib.rs",
                    "line": 2,
                    "column": 4,
                    "source": "lsp",
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["status"], "ok");
        assert_eq!(structured["symbol"]["name"], "widget");
        assert_eq!(structured["symbol"]["line"], 2);
        let references = structured["references"].as_array().unwrap();
        assert_eq!(references.len(), 2);
        assert!(
            references
                .iter()
                .any(|reference| reference["is_definition"] == true)
        );
        assert!(
            references
                .iter()
                .all(|reference| reference["kind"] == "function")
        );
        assert!(
            references
                .iter()
                .all(|reference| reference["source"] == "lsp")
        );
        assert!(output.content.contains("LSP · fake-lsp"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn high_level_diagnostics_use_pull_results_and_group_by_file() {
        use crate::tooling::code::CodeDiagnostics;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "diagnostics").await;
        let output = CodeDiagnostics
            .execute(
                &context(dir.path()),
                json!({"source": "lsp", "paths": ["src/lib.rs"], "severity": "all"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["status"], "ok");
        assert_eq!(structured["ok"], false);
        assert_eq!(structured["error_count"], 1);
        assert_eq!(structured["warning_count"], 1);
        assert_eq!(structured["groups"][0]["path"], "src/lib.rs");
        assert_eq!(
            structured["groups"][0]["diagnostics"][0]["server"],
            "fake-lsp"
        );
        assert_eq!(structured["groups"][0]["diagnostics"][0]["source"], "fake");
        assert!(output.content.contains("LSP · fake-lsp"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn high_level_repo_map_can_enrich_symbols_from_lsp() {
        use crate::tooling::code::RepoMap;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let output = RepoMap
            .execute(
                &context(dir.path()),
                json!({"source": "lsp", "include_tests": true}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["symbol_source"], "lsp");
        assert_eq!(structured["language_server"], "fake-lsp");
        assert!(
            structured["symbols"]
                .as_array()
                .is_some_and(|symbols| symbols.iter().all(|symbol| symbol["source"] == "lsp"))
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn high_level_build_context_can_enrich_symbols_from_lsp() {
        use crate::tooling::code::BuildContext;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let output = BuildContext
            .execute(
                &context(dir.path()),
                json!({"goal": "change widget", "source": "lsp", "max_tokens": 1000}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["symbol_source"], "lsp");
        assert!(
            structured["symbols"]
                .as_array()
                .is_some_and(|symbols| symbols.iter().all(|symbol| symbol["source"] == "lsp"))
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn lsp_query_status_and_capabilities_report_the_running_server() {
        let dir = fixture();
        let (connection, key, file) = fake_connection(dir.path(), "full").await;
        let status = LspQuery
            .execute(&context(dir.path()), json!({"action": "status"}))
            .await
            .unwrap();
        assert!(!status.is_error);
        let structured = status.structured.unwrap();
        assert_eq!(structured["running"], 1);
        assert_eq!(structured["source"], "lsp");
        let rust = structured["languages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|language| language["language"] == "rust")
            .unwrap();
        assert_eq!(rust["running"], true);
        assert_eq!(rust["server"], "fake-lsp");
        assert!(
            rust["capabilities"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "workspace_symbols"))
        );

        let capabilities = LspQuery
            .execute(
                &context(dir.path()),
                json!({"action": "capabilities", "path": "src/lib.rs"}),
            )
            .await
            .unwrap();
        assert!(!capabilities.is_error);
        let structured = capabilities.structured.unwrap();
        assert_eq!(structured["language_server"], "fake-lsp");
        assert_eq!(structured["capabilities"]["workspace_symbols"], true);
        assert!(
            structured["methods"]
                .as_array()
                .is_some_and(|methods| methods.iter().any(|method| method == "references"))
        );
        let _ = file;
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn stale_publish_version_is_not_accepted_for_a_newer_document() {
        let dir = fixture();
        let (connection, key, file) = fake_connection(dir.path(), "publish").await;
        let first = connection.open_document(&file).await.unwrap();
        let found = connection
            .wait_for_publish(std::slice::from_ref(&first), Duration::from_millis(500))
            .await;
        assert_eq!(found.len(), 1);

        let second = connection.open_document(&file).await.unwrap();
        assert!(second.version > first.version);
        let found = connection
            .wait_for_publish(std::slice::from_ref(&second), Duration::from_millis(100))
            .await;
        assert!(found.is_empty(), "stale publish must not be reused");
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn workspace_symbol_method_error_falls_back_to_document_symbols() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "workspace_error").await;
        assert!(connection.capabilities().workspace_symbols);
        let scope = vec![dir.path().join("src/lib.rs")];
        let scan = symbols_on_connection(&connection, "widget", &scope, 10)
            .await
            .unwrap();
        assert_eq!(scan.method, "textDocument/documentSymbol");
        assert_eq!(scan.status, Status::Partial);
        assert!(scan.symbols.iter().any(|symbol| symbol.name == "widget"));
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn definition_only_references_are_partial_and_report_the_method() {
        let dir = fixture();
        let (connection, key, file) = fake_connection(dir.path(), "definition_only").await;
        assert!(!connection.capabilities().references);
        assert!(connection.capabilities().definition);
        let scan = references_on_connection(
            &connection,
            &DeclarationPosition {
                path: file,
                line: 2,
                column: 4,
                kind: Some("function".into()),
            },
            true,
            10,
        )
        .await
        .unwrap();
        assert_eq!(scan.method, "textDocument/definition");
        assert_eq!(scan.status, Status::Partial);
        assert_eq!(scan.locations.len(), 1);
        assert!(scan.locations[0].is_definition);
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn lsp_query_definition_hover_symbols_workspace_symbols_and_diagnostics() {
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "diagnostics").await;
        let definition = LspQuery
            .execute(
                &context(dir.path()),
                json!({"action": "definition", "path": "src/lib.rs", "line": 2, "column": 4}),
            )
            .await
            .unwrap();
        assert!(!definition.is_error, "{}", definition.content);
        let structured = definition.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["locations"][0]["line"], 2);
        assert_eq!(structured["locations"][0]["column"], 4);
        assert_eq!(structured["locations"][0]["source"], "lsp");

        let hover = LspQuery
            .execute(
                &context(dir.path()),
                json!({"action": "hover", "path": "src/lib.rs", "line": 2, "column": 4}),
            )
            .await
            .unwrap();
        assert!(!hover.is_error);
        let structured = hover.structured.unwrap();
        assert!(structured["hover"].as_str().unwrap().contains("widget"));
        assert_eq!(structured["range"]["line"], 2);
        assert_eq!(structured["range"]["column"], 4);

        let symbols = LspQuery
            .execute(
                &context(dir.path()),
                json!({"action": "symbols", "path": "src/lib.rs"}),
            )
            .await
            .unwrap();
        assert!(!symbols.is_error);
        let structured = symbols.structured.unwrap();
        let symbols = structured["symbols"].as_array().unwrap();
        assert_eq!(symbols[0]["kind"], "function");
        assert!(symbols.iter().any(|symbol| symbol["name"] == "helper"));
        let widget = symbols
            .iter()
            .find(|symbol| symbol["name"] == "Widget")
            .unwrap();
        assert_eq!(widget["kind"], "struct");
        assert_eq!(widget["kind_group"], "type");

        let workspace = LspQuery
            .execute(
                &context(dir.path()),
                json!({"action": "workspace_symbols", "language": "Rust", "query": "widget", "limit": 6}),
            )
            .await
            .unwrap();
        assert!(!workspace.is_error);
        let structured = workspace.structured.unwrap();
        assert_eq!(structured["symbols"].as_array().unwrap().len(), 6);
        assert_eq!(structured["symbols"][0]["name"], "widget");

        let diagnostics = LspQuery
            .execute(
                &context(dir.path()),
                json!({"action": "diagnostics", "path": "src/lib.rs"}),
            )
            .await
            .unwrap();
        assert!(!diagnostics.is_error, "{}", diagnostics.content);
        let structured = diagnostics.structured.unwrap();
        assert_eq!(structured["error_count"], 1);
        assert_eq!(structured["warning_count"], 1);
        assert_eq!(structured["groups"][0]["path"], "src/lib.rs");
        assert_eq!(
            structured["groups"][0]["diagnostics"][0]["server"],
            "fake-lsp"
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn lsp_query_missing_path_and_unknown_action_are_structured_errors() {
        let dir = fixture();
        let missing = LspQuery
            .execute(&context(dir.path()), json!({"action": "definition"}))
            .await
            .unwrap();
        assert!(missing.is_error);
        assert_eq!(missing.structured.unwrap()["code"], "INVALID_ARGUMENT");

        let unknown = LspQuery
            .execute(&context(dir.path()), json!({"action": "rename"}))
            .await
            .unwrap();
        assert!(unknown.is_error);
        let structured = unknown.structured.unwrap();
        assert_eq!(structured["code"], "INVALID_ARGUMENT");
        assert_eq!(structured["action"], "rename");
    }

    #[tokio::test]
    async fn high_level_diagnostics_accept_push_results() {
        use crate::tooling::code::CodeDiagnostics;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "publish").await;
        let output = CodeDiagnostics
            .execute(
                &context(dir.path()),
                json!({"source": "lsp", "paths": ["src/lib.rs"], "severity": "all"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["status"], "ok");
        assert_eq!(structured["error_count"], 1);
        assert_eq!(
            structured["groups"][0]["diagnostics"][0]["message"],
            "published error"
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_by_the_uri_jail() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(dir.path().join("outside.rs"), "fn secret() {}\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("outside.rs"), root.join("src/escape.rs"))
            .unwrap();
        let uri = reqwest::Url::from_file_path(root.join("src/escape.rs"))
            .unwrap()
            .to_string();
        assert!(uri_to_path(&root, &uri).is_none());
    }

    #[test]
    fn flat_document_symbols_map_paths_and_drop_outside_files() {
        let dir = fixture();
        let root = dir.path();
        let inside = reqwest::Url::from_file_path(root.join("src/lib.rs"))
            .unwrap()
            .to_string();
        let value = json!([
            {
                "name": "Widget",
                "kind": 23,
                "location": {
                    "uri": inside,
                    "range": {"start": {"line": 4, "character": 7}, "end": {"line": 4, "character": 13}}
                }
            },
            {
                "name": "escape",
                "kind": 12,
                "location": {
                    "uri": "file:///etc/passwd",
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}
                }
            }
        ]);
        let mut cache = SourceCache::new(root);
        let symbols = document_symbols_from_value(
            &value,
            &mut cache,
            root,
            &root.join("src/lib.rs"),
            "Rust",
            10,
        );
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Widget");
        assert_eq!(symbols[0].path, "src/lib.rs");
        assert_eq!(symbols[0].line, 5);
        assert_eq!(symbols[0].column, 8);
        assert_eq!(symbols[0].kind_group, "type");
    }

    #[test]
    fn diagnostics_map_multiple_files_and_drop_outside_files() {
        let dir = fixture();
        let root = dir.path();
        let first = reqwest::Url::from_file_path(root.join("src/lib.rs"))
            .unwrap()
            .to_string();
        let second = reqwest::Url::from_file_path(root.join("src/other.rs"))
            .unwrap()
            .to_string();
        let items = json!([
            {
                "uri": first,
                "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 4}},
                "severity": 1,
                "message": "first",
                "source": "fake"
            },
            {
                "uri": second,
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
                "severity": 2,
                "message": "second",
                "source": "fake"
            },
            {
                "uri": "file:///etc/passwd",
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 1,
                "message": "escape"
            }
        ]);
        let mut cache = SourceCache::new(root);
        let diagnostics = diagnostics_from_items(
            items.as_array().unwrap(),
            &mut cache,
            root,
            None,
            Some("fake-lsp"),
            50,
        );
        let summary = prepare_diagnostics(diagnostics, 50);
        assert_eq!(summary.groups.len(), 2);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.warnings, 1);
        assert!(
            summary
                .groups
                .iter()
                .all(|group| !group.path.contains("passwd"))
        );
    }

    #[tokio::test]
    async fn references_can_exclude_the_declaration() {
        use crate::tooling::code::FindReferences;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "full").await;
        let output = FindReferences
            .execute(
                &context(dir.path()),
                json!({
                    "name": "widget",
                    "path": "src/lib.rs",
                    "line": 2,
                    "column": 4,
                    "source": "lsp",
                    "include_declaration": false,
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        let references = structured["references"].as_array().unwrap();
        assert!(!references.is_empty());
        assert!(
            references
                .iter()
                .all(|reference| reference["is_definition"] != true)
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn high_level_diagnostics_severity_filter_is_applied_to_lsp_results() {
        use crate::tooling::code::CodeDiagnostics;
        let dir = fixture();
        let (connection, key, _) = fake_connection(dir.path(), "diagnostics").await;
        let output = CodeDiagnostics
            .execute(
                &context(dir.path()),
                json!({"source": "lsp", "paths": ["src/lib.rs"], "severity": "warning"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["error_count"], 0);
        assert_eq!(structured["warning_count"], 1);
        assert_eq!(
            structured["groups"][0]["diagnostics"][0]["severity"],
            "warning"
        );
        kill_server(&connection.server);
        registry().lock().unwrap().remove(&key);
    }

    #[tokio::test]
    async fn repeated_start_failures_enter_a_bounded_cooldown() {
        let dir = fixture();
        let context = context(dir.path());
        static BROKEN: LspLanguage = LspLanguage {
            id: "broken-test",
            display: "Broken",
            extensions: &["broken"],
            candidates: &[("python3", &["-c", "import sys; sys.exit(1)"])],
            install_hint: "install a working test server",
        };
        let first = match connect(&context, BROKEN).await {
            Ok(_) => panic!("expected the broken test server to fail"),
            Err(error) => error,
        };
        assert!(matches!(first, LspConnectError::StartFailed { .. }));
        assert_eq!(first.code(), "LSP_START_FAILED");
        let second = match connect(&context, BROKEN).await {
            Ok(_) => panic!("expected cooldown"),
            Err(error) => error,
        };
        match second {
            LspConnectError::Cooldown {
                retry_after_secs,
                message,
                ..
            } => {
                assert!(retry_after_secs >= 1);
                assert!(!message.is_empty());
            }
            other => panic!("expected cooldown, got {other:?}"),
        }
        // A worktree reset clears the cooldown so a repaired server can retry.
        stop_lsp_servers(dir.path());
        let third = match connect(&context, BROKEN).await {
            Ok(_) => panic!("expected another start failure after reset"),
            Err(error) => error,
        };
        assert!(matches!(third, LspConnectError::StartFailed { .. }));
    }

    #[test]
    fn status_report_never_spawns_and_labels_install_state() {
        let dir = TempDir::new().unwrap();
        let report = status_report(dir.path());
        assert_eq!(report.languages.len(), all_languages().len());
        assert!(report.languages.iter().all(|language| !language.running));
        assert!(
            report
                .languages
                .iter()
                .any(|language| language.language == "rust")
        );
        let rust = report
            .languages
            .iter()
            .find(|language| language.language == "rust")
            .unwrap();
        assert!(!rust.install_hint.is_empty());
    }
}

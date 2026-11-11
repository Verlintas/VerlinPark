//! Deterministic code intelligence for agent tools.
//!
//! The lexical implementation is a bounded index, not a full language server.
//! It extracts symbols with per-language patterns and resolves imports where a
//! project layout makes that unambiguous. Every result reports lexical
//! confidence so the model does not confuse it with compiler-accurate
//! navigation. An installed language server is the optional exact backend; the
//! high-level tools label LSP, lexical, and build results separately (ADR-0171).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::tooling::code_intel::{
    BUILD_CONFIDENCE, Backend, CodeDiagnostic, CodeLocation, CodeRange, CodeSymbol,
    LEXICAL_CONFIDENCE_REFERENCE, LEXICAL_CONFIDENCE_SYMBOL, LSP_CONFIDENCE_SYMBOL, SourceChoice,
    Status, backend_sentence, display_language, kind_group, prepare_diagnostics, prepare_locations,
    prepare_symbols, render_diagnostics, render_locations, render_symbols, score_name_match,
};
use crate::tooling::lsp::{self, DeclarationPosition, LspAttemptError, MAX_DIAGNOSTIC_FILES};
use crate::tooling::project::{
    AffectedTest, affected_tests, detect_profile, git_changed_paths, profile_affected_candidates,
    run_detected_task,
};
use crate::tools::{AgentTool, ToolContext, ToolOutput, safe_existing_path, truncate};

const MAX_INDEX_FILES: usize = 5_000;
const MAX_INDEX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_REPO_MAP_SYMBOLS: usize = 800;
const MAX_FIND_RESULTS: usize = 100;
const MAX_REFERENCES: usize = 200;
const MAX_GRAPH_NODES: usize = 200;
const MAX_SCAN_FILES: usize = 2_000;
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const INDEX_TTL: Duration = Duration::from_secs(20);
const MAX_CONTEXT_TOKENS: usize = 16_000;

pub struct RepoMap;
pub struct FindSymbol;
pub struct FindReferences;
pub struct CodeDiagnostics;
pub struct AffectedTests;
pub struct DependencyGraph;
pub struct BuildContext;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Symbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub language: String,
    pub path: String,
    pub line: usize,
    pub end_line: usize,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
struct LanguageCount {
    name: String,
    files: usize,
    symbols: usize,
}

#[derive(Debug, Clone)]
struct IndexedFile {
    path: String,
    language: &'static str,
    bytes: u64,
    is_test: bool,
    is_config: bool,
}

#[derive(Debug, Clone, Default)]
struct CodeIndex {
    root: PathBuf,
    files: Vec<IndexedFile>,
    symbols: Vec<Symbol>,
    languages: Vec<LanguageCount>,
    truncated: bool,
    build_ms: u128,
}

type IndexCache = HashMap<PathBuf, (Instant, Arc<CodeIndex>)>;

static INDEX_CACHE: OnceLock<Mutex<IndexCache>> = OnceLock::new();

/// Drop the lexical index for a worktree after a write. Shell commands and
/// file tools invalidate eagerly because they can change any number of files.
pub(crate) fn invalidate_index(root: &Path) {
    let cache = INDEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if let Ok(mut cache) = cache.lock() {
        cache.remove(&key);
    }
}

fn cached_index(root: &Path) -> Arc<CodeIndex> {
    let cache = INDEX_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if let Ok(cache) = cache.lock()
        && let Some((built, index)) = cache.get(&key)
        && built.elapsed() < INDEX_TTL
    {
        return index.clone();
    }
    let index = Arc::new(build_index(&key));
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= 32 {
            let oldest = cache
                .iter()
                .min_by_key(|(_, (built, _))| *built)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(key, (Instant::now(), index.clone()));
    }
    index
}

fn build_index(root: &Path) -> CodeIndex {
    let started = Instant::now();
    let mut files = Vec::new();
    let mut symbols = Vec::new();
    let mut truncated = false;
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .max_depth(Some(20))
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git"
                    | "target"
                    | "node_modules"
                    | "dist"
                    | "build"
                    | ".build"
                    | "DerivedData"
                    | "Pods"
                    | "vendor"
                    | ".venv"
                    | "venv"
                    | "coverage"
            )
        });
    for entry in builder.build().flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(language) = language_for_file(path) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_INDEX_FILE_BYTES {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let relative = relative_path(root, path);
        let is_test = is_test_path(&relative);
        let is_config = is_config_file(&relative);
        if is_config {
            files.push(IndexedFile {
                path: relative.clone(),
                language,
                bytes: metadata.len(),
                is_test,
                is_config,
            });
            continue;
        }
        symbols.extend(extract_symbols(root, path, language, &content));
        files.push(IndexedFile {
            path: relative,
            language,
            bytes: metadata.len(),
            is_test,
            is_config,
        });
        if files.len() >= MAX_INDEX_FILES {
            truncated = true;
            break;
        }
    }
    symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut language_map: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    for file in &files {
        let entry = language_map.entry(file.language).or_default();
        entry.0 += 1;
    }
    for symbol in &symbols {
        if let Some((_, count)) = language_map.get_mut(symbol.language.as_str()) {
            *count += 1;
        }
    }
    let languages = language_map
        .into_iter()
        .map(|(name, (files, symbols))| LanguageCount {
            name: name.to_string(),
            files,
            symbols,
        })
        .collect();
    CodeIndex {
        root: root.to_path_buf(),
        files,
        symbols,
        languages,
        truncated,
        build_ms: started.elapsed().as_millis(),
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn language_for_file(path: &Path) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("Rust"),
        "swift" => Some("Swift"),
        "ts" | "tsx" | "mts" | "cts" => Some("TypeScript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
        "py" | "pyi" => Some("Python"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "c" | "h" => Some("C"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => Some("C++"),
        "cs" => Some("C#"),
        "rb" => Some("Ruby"),
        "php" => Some("PHP"),
        _ => None,
    }
}

pub(crate) fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.py")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.ends_with("tests.swift")
        || lower.ends_with("test.swift")
}

fn is_config_file(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some(
            "Cargo.toml"
                | "package.json"
                | "pyproject.toml"
                | "requirements.txt"
                | "go.mod"
                | "Package.swift"
                | "project.yml"
                | "Makefile"
                | "justfile"
                | "Justfile"
                | "tsconfig.json"
                | "vite.config.ts"
                | "next.config.js"
                | "CMakeLists.txt"
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "docker-compose.yml"
                | "Dockerfile"
        )
    )
}

fn extract_symbols(root: &Path, path: &Path, language: &'static str, content: &str) -> Vec<Symbol> {
    let relative = relative_path(root, path);
    let lines = content.lines().collect::<Vec<_>>();
    let mut symbols = Vec::new();
    let mut python_scope: Vec<(usize, String)> = Vec::new();
    let mut rust_impl: Option<(String, usize)> = None;
    for (index, raw) in lines.iter().enumerate() {
        let line_number = index + 1;
        if rust_impl
            .as_ref()
            .is_some_and(|(_, end)| line_number > *end)
        {
            rust_impl = None;
        }
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("<!--")
            || trimmed.starts_with("--")
        {
            continue;
        }
        if language == "Python" {
            let indent = raw.len() - raw.trim_start().len();
            while python_scope
                .last()
                .is_some_and(|(scope_indent, _)| *scope_indent >= indent)
            {
                python_scope.pop();
            }
            if let Some(name) = python_class(trimmed) {
                let kind = "class";
                let container = python_scope.last().map(|(_, name)| name.clone());
                let end_line = python_end_line(&lines, index, indent);
                symbols.push(symbol(
                    &relative,
                    language,
                    &name,
                    kind,
                    line_number,
                    end_line,
                    trimmed,
                    container,
                    visibility(trimmed),
                ));
                python_scope.push((indent, name));
                continue;
            }
            if let Some(name) = python_def(trimmed) {
                let container = python_scope.last().map(|(_, name)| name.clone());
                let end_line = python_end_line(&lines, index, indent);
                symbols.push(symbol(
                    &relative,
                    language,
                    &name,
                    "function",
                    line_number,
                    end_line,
                    trimmed,
                    container,
                    visibility(trimmed),
                ));
                python_scope.push((indent, name));
                continue;
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("impl") {
            let name = rest
                .split(['{', '<'])
                .next()
                .unwrap_or_default()
                .trim()
                .trim_end_matches(':')
                .to_string();
            if !name.is_empty() {
                let end_line = brace_end_line(&lines, index);
                symbols.push(symbol(
                    &relative,
                    language,
                    &name,
                    "impl",
                    line_number,
                    end_line,
                    trimmed,
                    None,
                    visibility(trimmed),
                ));
                rust_impl = Some((name, end_line));
            }
            continue;
        }
        let candidates = match language {
            "Rust" => rust_symbol(trimmed),
            "Swift" => swift_symbol(trimmed),
            "TypeScript" | "JavaScript" => js_symbol(trimmed),
            "Go" => go_symbol(trimmed),
            "Java" | "Kotlin" => jvm_symbol(trimmed),
            "C" | "C++" | "C#" => c_symbol(trimmed),
            _ => fallback_symbol(trimmed),
        };
        for (name, kind) in candidates {
            let end_line = if matches!(
                language,
                "Rust"
                    | "Swift"
                    | "TypeScript"
                    | "JavaScript"
                    | "Go"
                    | "Java"
                    | "Kotlin"
                    | "C"
                    | "C++"
                    | "C#"
            ) {
                brace_end_line(&lines, index)
            } else {
                line_number
            };
            let container = if language == "Rust" {
                rust_impl.as_ref().map(|(name, _)| name.clone())
            } else {
                None
            };
            symbols.push(symbol(
                &relative,
                language,
                &name,
                &kind,
                line_number,
                end_line,
                trimmed,
                container,
                visibility(trimmed),
            ));
        }
    }
    symbols
}

#[allow(clippy::too_many_arguments)]
fn symbol(
    path: &str,
    language: &str,
    name: &str,
    kind: &str,
    line: usize,
    end_line: usize,
    signature: &str,
    container: Option<String>,
    visibility: Option<String>,
) -> Symbol {
    Symbol {
        id: format!("{path}:{line}:{name}"),
        name: name.to_string(),
        kind: kind.to_string(),
        language: language.to_string(),
        path: path.to_string(),
        line,
        end_line: end_line.max(line),
        signature: signature.chars().take(240).collect(),
        container,
        visibility,
        confidence: 0.75,
    }
}

fn visibility(line: &str) -> Option<String> {
    if line.contains("pub ")
        || line.contains("public ")
        || line.contains("open ")
        || line.starts_with("export ")
    {
        Some("public".to_string())
    } else {
        None
    }
}

fn python_end_line(lines: &[&str], start: usize, indent: usize) -> usize {
    let mut end = start + 1;
    let mut index = start + 1;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        let current_indent = line.len() - line.trim_start().len();
        if current_indent <= indent {
            break;
        }
        end = index + 1;
        index += 1;
    }
    end
}

fn brace_end_line(lines: &[&str], start: usize) -> usize {
    let mut depth = 0isize;
    let mut seen = false;
    for (index, line) in lines.iter().enumerate().skip(start).take(1_000) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    seen = true;
                }
                '}' => {
                    depth -= 1;
                    if seen && depth <= 0 {
                        return index + 1;
                    }
                }
                _ => {}
            }
        }
        if !seen && index > start {
            // One-line declarations without braces, or signatures spanning a
            // few lines. Stop at a blank line to avoid swallowing the file.
            if line.trim().is_empty() {
                return index;
            }
        }
    }
    if seen { lines.len() } else { start + 1 }
}

fn python_class(line: &str) -> Option<String> {
    regex_capture_at(r"^class\s+([A-Za-z_][A-Za-z0-9_]*)", line, 1)
}

fn python_def(line: &str) -> Option<String> {
    regex_capture_at(r"^(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)", line, 1)
}

fn rust_symbol(line: &str) -> Vec<(String, String)> {
    if let Some(name) = regex_capture_at(
        r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "function".to_string())];
    }
    if let Some(name) = regex_capture_at(
        r"^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|union|type)\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "type".to_string())];
    }
    if let Some(name) = regex_capture_at(
        r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "module".to_string())];
    }
    if let Some(name) = regex_capture_at(
        r"^(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "constant".to_string())];
    }
    if let Some(name) = regex_capture_at(r"^macro_rules!\s+([A-Za-z_][A-Za-z0-9_]*)", line, 1) {
        return vec![(name, "macro".to_string())];
    }
    Vec::new()
}

fn swift_symbol(line: &str) -> Vec<(String, String)> {
    let prefixes = r"^(?:@\w+\s+)*(?:(?:public|private|internal|fileprivate|open|final|static|class|override|mutating|nonmutating|convenience|required|async|throws)\s+)*";
    if let Some(name) = regex_capture_at(
        &format!(r"{prefixes}func\s+([A-Za-z_][A-Za-z0-9_]*)"),
        line,
        1,
    ) {
        return vec![(name, "function".to_string())];
    }
    if let Some(name) = regex_capture_at(
        &format!(
            r"{prefixes}(?:class|struct|enum|protocol|extension|actor|typealias)\s+([A-Za-z_][A-Za-z0-9_]*)"
        ),
        line,
        1,
    ) {
        return vec![(name, "type".to_string())];
    }
    if let Some(name) = regex_capture_at(
        &format!(r"{prefixes}(?:var|let)\s+([A-Za-z_][A-Za-z0-9_]*)"),
        line,
        1,
    ) {
        return vec![(name, "constant".to_string())];
    }
    Vec::new()
}

fn js_symbol(line: &str) -> Vec<(String, String)> {
    if let Some(name) = regex_capture_at(
        r"^(?:export\s+)?(?:default\s+)?(?:async\s+)?(?:function|class|interface|type|enum)\s+([A-Za-z_$][A-Za-z0-9_$]*)",
        line,
        1,
    ) {
        return vec![(name, "symbol".to_string())];
    }
    if let Some(name) = regex_capture_at(
        r"^(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][A-Za-z0-9_$]*)\s*=>",
        line,
        1,
    ) {
        return vec![(name, "function".to_string())];
    }
    Vec::new()
}

fn go_symbol(line: &str) -> Vec<(String, String)> {
    if let Some(name) = regex_capture_at(
        r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "function".to_string())];
    }
    if let Some(name) = regex_capture_at(
        r"^type\s+([A-Za-z_][A-Za-z0-9_]*)\s+(?:struct|interface|func|map|chan|\[)",
        line,
        1,
    ) {
        return vec![(name, "type".to_string())];
    }
    Vec::new()
}

fn jvm_symbol(line: &str) -> Vec<(String, String)> {
    if let Some(name) = regex_capture_at(
        r"^(?:(?:public|private|protected|internal|static|final|abstract|open|data|sealed|override|suspend|inline|operator)\s+)*(?:class|interface|enum|record|object)\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "type".to_string())];
    }
    if let Some(name) = regex_capture_at(
        r"^(?:(?:public|private|protected|internal|static|final|abstract|open|override|suspend|inline|operator)\s+)*fun\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "function".to_string())];
    }
    Vec::new()
}

fn c_symbol(line: &str) -> Vec<(String, String)> {
    if let Some(name) = regex_capture_at(
        r"^(?:typedef\s+)?(?:class|struct|enum|union|namespace)\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        return vec![(name, "type".to_string())];
    }
    if line.contains('(')
        && !line.ends_with(';')
        && let Some(name) = regex_capture_at(
            r"^(?:[A-Za-z_][A-Za-z0-9_:<>,\*&\s]+)\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
            line,
            1,
        )
        && !matches!(
            name.as_str(),
            "if" | "for" | "while" | "switch" | "return" | "sizeof"
        )
    {
        return vec![(name, "function".to_string())];
    }
    Vec::new()
}

fn fallback_symbol(line: &str) -> Vec<(String, String)> {
    if let Some(name) = regex_capture_at(
        r"^(?:def|func|fn|function|class|struct|interface|enum|module)\s+([A-Za-z_][A-Za-z0-9_]*)",
        line,
        1,
    ) {
        vec![(name, "symbol".to_string())]
    } else {
        Vec::new()
    }
}

fn compiled_regex(pattern: &str) -> Option<Regex> {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some(regex) = cache.get(pattern)
    {
        return Some(regex.clone());
    }
    let regex = Regex::new(pattern).ok()?;
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(pattern.to_string(), regex.clone());
    }
    Some(regex)
}

fn regex_capture_at(pattern: &str, line: &str, group: usize) -> Option<String> {
    let regex = compiled_regex(pattern)?;
    regex
        .captures(line)
        .and_then(|captures| captures.get(group))
        .map(|value| value.as_str().to_string())
}

#[async_trait]
impl AgentTool for RepoMap {
    fn name(&self) -> &'static str {
        "repo_map"
    }

    fn description(&self) -> &'static str {
        "Return a compact map of repository languages, files, public symbols, tests, and config boundaries. Symbols are lexical by default; source=auto uses an already-running language server, source=lsp asks an installed one and labels exact results."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory relative to the worktree root; default ."},
                "max_files": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "Default 2000"},
                "max_symbols": {"type": "integer", "minimum": 1, "maximum": 800, "description": "Default 300"},
                "include_tests": {"type": "boolean", "description": "Include test files and symbols; default false"},
                "source": {"type": "string", "enum": ["auto", "lsp", "lexical"], "description": "Default lexical; auto only uses an already-running server"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = safe_existing_path(&context.root, relative)?;
        if !root.is_dir() {
            return Ok(ToolOutput::error_structured(
                "repo_map path must be a directory",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let source = match parse_source_choice(&input, SourceChoice::Lexical) {
            Ok(source) => source,
            Err(output) => return Ok(*output),
        };
        if source == SourceChoice::Build {
            return Ok(ToolOutput::error_structured(
                "repo_map does not have a build backend; use lexical or lsp",
                json!({"code": "INVALID_ARGUMENT", "field": "source"}),
            ));
        }
        let max_files = input
            .get("max_files")
            .and_then(Value::as_u64)
            .unwrap_or(2_000)
            .clamp(1, MAX_INDEX_FILES as u64) as usize;
        let max_symbols = input
            .get("max_symbols")
            .and_then(Value::as_u64)
            .unwrap_or(300)
            .clamp(1, MAX_REPO_MAP_SYMBOLS as u64) as usize;
        let include_tests = input
            .get("include_tests")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let index = cached_index(&root);
        let files = index
            .files
            .iter()
            .filter(|file| include_tests || !file.is_test)
            .take(max_files)
            .collect::<Vec<_>>();
        let file_set = files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<HashSet<_>>();
        let mut symbols = index
            .symbols
            .iter()
            .filter(|symbol| file_set.contains(symbol.path.as_str()))
            .filter(|symbol| include_tests || !is_test_path(&symbol.path))
            .map(|symbol| lexical_code_symbol(&root, symbol, symbol.confidence))
            .collect::<Vec<_>>();
        let lexical_symbol_count = symbols.len();
        let mut backend = Backend::lexical(Status::Ok, LEXICAL_CONFIDENCE_SYMBOL);
        let mut symbol_source = "lexical";
        let mut lsp_attempt_list = Vec::new();
        if source != SourceChoice::Lexical {
            let lsp_files = files
                .iter()
                .map(|file| context.root.join(&file.path))
                .collect::<Vec<_>>();
            let allow_spawn = source == SourceChoice::Lsp;
            match lsp::collect_file_symbols(context, &lsp_files, max_symbols, allow_spawn).await {
                Ok(batch) => {
                    let mut enriched = batch.symbols;
                    if !include_tests {
                        enriched.retain(|symbol| !is_test_path(&symbol.path));
                    }
                    let (enriched, _) = prepare_symbols(enriched, None, max_symbols);
                    lsp_attempt_list = lsp_attempts(&batch.attempts);
                    if !enriched.is_empty() {
                        symbols = enriched;
                        symbol_source = "lsp";
                        backend = backend_for_lsp_batch(
                            SourceChoice::Lsp,
                            batch.status,
                            LSP_CONFIDENCE_SYMBOL,
                            &batch.attempts,
                        );
                    } else {
                        backend = Backend::lexical(Status::Fallback, LEXICAL_CONFIDENCE_SYMBOL)
                            .with_fallback("lsp_empty", None)
                            .with_attempts(lsp_attempt_list.clone());
                    }
                }
                Err(error) => {
                    if source == SourceChoice::Lsp {
                        return Ok(lsp_unavailable_output(&error, "repo_map"));
                    }
                    backend = lexical_fallback_backend(&error);
                }
            }
        }
        let (symbols, capped) = prepare_symbols(symbols, None, max_symbols);
        let tests = index
            .files
            .iter()
            .filter(|file| file.is_test)
            .take(100)
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let configs = index
            .files
            .iter()
            .filter(|file| file.is_config)
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let rendered = render_symbols(&symbols);
        let language_names = index
            .languages
            .iter()
            .take(5)
            .map(|language| format!("{} ({})", language.name, language.files))
            .collect::<Vec<_>>()
            .join(", ");
        let summary = format!(
            "{} files · {} languages{} · {} symbols · {} tests · {}",
            files.len(),
            index.languages.len(),
            if language_names.is_empty() {
                String::new()
            } else {
                format!(" ({language_names})")
            },
            symbols.len(),
            tests.len(),
            backend_sentence(&backend)
        );
        let mut structured = json!({
            "root": index.root,
            "files": files.iter().map(|file| json!({
                "path": file.path,
                "language": file.language,
                "bytes": file.bytes,
                "is_test": file.is_test,
                "is_config": file.is_config,
            })).collect::<Vec<_>>(),
            "languages": index.languages,
            "symbols": symbols,
            "tests": tests,
            "configs": configs,
            "summary": summary,
            "truncated": index.truncated || files.len() >= max_files || capped,
            "symbol_source": symbol_source,
            "lsp_symbols": if symbol_source == "lsp" { symbols.len() } else { 0 },
            "lexical_symbols": if symbol_source == "lsp" { 0 } else { lexical_symbol_count },
            "lsp_attempts": lsp_attempt_list,
            "build_ms": index.build_ms,
        });
        backend.apply(&mut structured);
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n{rendered}")),
            structured,
        ))
    }
}

#[async_trait]
impl AgentTool for FindSymbol {
    fn name(&self) -> &'static str {
        "find_symbol"
    }

    fn description(&self) -> &'static str {
        "Find a function, type, module, or other symbol by name with path, range, signature, and optional body. Uses an installed language server when source=auto/lsp and labels every result source and confidence."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Symbol name or substring"},
                "kind": {"type": "string", "description": "Optional kind or kind group filter such as function, type, module"},
                "language": {"type": "string", "description": "Optional language filter"},
                "path": {"type": "string", "description": "Optional directory filter"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Default 20"},
                "include_body": {"type": "boolean", "description": "Include the symbol body; default false"},
                "source": {"type": "string", "enum": ["auto", "lsp", "lexical"], "description": "Default auto; falls back to lexical only when explicitly labeled"}
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| anyhow!("query is required"))?;
        let source = match parse_source_choice(&input, SourceChoice::Auto) {
            Ok(source) => source,
            Err(output) => return Ok(*output),
        };
        if source == SourceChoice::Build {
            return Ok(ToolOutput::error_structured(
                "find_symbol does not have a build backend; use auto, lsp, or lexical",
                json!({"code": "INVALID_ARGUMENT", "field": "source"}),
            ));
        }
        let index = cached_index(&context.root);
        let kind = input.get("kind").and_then(Value::as_str);
        let language = input.get("language").and_then(Value::as_str);
        let path_filter = input.get("path").and_then(Value::as_str);
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, MAX_FIND_RESULTS as u64) as usize;
        let include_body = input
            .get("include_body")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if source != SourceChoice::Lexical {
            let scope_files = index
                .files
                .iter()
                .filter(|file| path_filter.is_none_or(|path| file.path.starts_with(path)))
                .filter(|file| {
                    language.is_none_or(|language| {
                        file.language.eq_ignore_ascii_case(language)
                            || lsp::language_for_path(Path::new(&file.path)).is_some_and(|server| {
                                server.id.eq_ignore_ascii_case(language)
                                    || server.display.eq_ignore_ascii_case(language)
                            })
                    })
                })
                .map(|file| context.root.join(&file.path))
                .collect::<Vec<_>>();
            let languages = lsp::languages_for_files(&scope_files, 3);
            if !languages.is_empty() {
                match lsp::search_symbols(context, &languages, query, &scope_files, limit, true)
                    .await
                {
                    Ok(batch) => {
                        let symbols = batch
                            .symbols
                            .into_iter()
                            .filter(|symbol| symbol_filter(symbol, kind, language, path_filter))
                            .collect::<Vec<_>>();
                        let (symbols, capped) = prepare_symbols(symbols, Some(query), limit);
                        let backend = backend_for_lsp_batch(
                            SourceChoice::Lsp,
                            batch.status,
                            LSP_CONFIDENCE_SYMBOL,
                            &batch.attempts,
                        );
                        let values = symbol_values(&context.root, &symbols, include_body);
                        let rendered =
                            render_symbols_with_body(&context.root, &symbols, include_body);
                        let heading = symbol_heading(&backend, query, symbols.len());
                        let text = if rendered.is_empty() {
                            format!("{heading}\nno symbols matched {query}")
                        } else {
                            format!("{heading}\n{rendered}")
                        };
                        let mut structured = json!({
                            "query": query,
                            "symbols": values,
                            "count": symbols.len(),
                            "truncated": batch.truncated || capped,
                        });
                        backend.apply(&mut structured);
                        return Ok(ToolOutput::ok_structured(truncate(text), structured));
                    }
                    Err(error) => {
                        if source == SourceChoice::Lsp {
                            return Ok(lsp_unavailable_output(&error, "find_symbol"));
                        }
                        let backend = lexical_fallback_backend(&error);
                        return Ok(lexical_find_symbol_output(
                            context,
                            &index,
                            query,
                            kind,
                            language,
                            path_filter,
                            limit,
                            include_body,
                            backend,
                        ));
                    }
                }
            } else if source == SourceChoice::Lsp {
                let error = LspAttemptError::NoLanguage {
                    supported: lsp::all_languages()
                        .iter()
                        .map(|language| language.id.to_string())
                        .collect(),
                };
                return Ok(lsp_unavailable_output(&error, "find_symbol"));
            } else {
                let error = LspAttemptError::NoLanguage {
                    supported: lsp::all_languages()
                        .iter()
                        .map(|language| language.id.to_string())
                        .collect(),
                };
                let backend = lexical_fallback_backend(&error);
                return Ok(lexical_find_symbol_output(
                    context,
                    &index,
                    query,
                    kind,
                    language,
                    path_filter,
                    limit,
                    include_body,
                    backend,
                ));
            }
        }
        let backend = Backend::lexical(Status::Ok, LEXICAL_CONFIDENCE_SYMBOL);
        Ok(lexical_find_symbol_output(
            context,
            &index,
            query,
            kind,
            language,
            path_filter,
            limit,
            include_body,
            backend,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn lexical_find_symbol_output(
    context: &ToolContext,
    index: &CodeIndex,
    query: &str,
    kind: Option<&str>,
    language: Option<&str>,
    path_filter: Option<&str>,
    limit: usize,
    include_body: bool,
    backend: Backend,
) -> ToolOutput {
    let symbols = index
        .symbols
        .iter()
        .filter(|symbol| {
            kind.is_none_or(|kind| symbol.kind == kind || kind_group(&symbol.kind) == kind)
        })
        .filter(|symbol| {
            language.is_none_or(|language| symbol.language.eq_ignore_ascii_case(language))
        })
        .filter(|symbol| path_filter.is_none_or(|path| symbol.path.starts_with(path)))
        .filter_map(|symbol| {
            let score = score_name_match(&symbol.name, query);
            (score > 0.0).then(|| lexical_code_symbol(&context.root, symbol, score))
        })
        .collect::<Vec<_>>();
    let (mut symbols, capped) = prepare_symbols(symbols, Some(query), limit);
    symbols.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    let values = symbol_values(&context.root, &symbols, include_body);
    let rendered = render_symbols_with_body(&context.root, &symbols, include_body);
    let heading = symbol_heading(&backend, query, symbols.len());
    let text = if rendered.is_empty() {
        format!("{heading}\nno symbols matched {query}")
    } else {
        format!("{heading}\n{rendered}")
    };
    let mut structured = json!({
        "query": query,
        "symbols": values,
        "count": symbols.len(),
        "truncated": capped,
    });
    backend.apply(&mut structured);
    ToolOutput::ok_structured(truncate(text), structured)
}

fn read_code_body(root: &Path, path: &str, line: u64, end_line: u64) -> Option<String> {
    let content = std::fs::read_to_string(root.join(path)).ok()?;
    let lines = content.lines().collect::<Vec<_>>();
    let start = usize::try_from(line.saturating_sub(1))
        .ok()?
        .min(lines.len());
    let end = usize::try_from(end_line).ok()?.min(lines.len());
    if start >= end {
        return Some(String::new());
    }
    Some(lines[start..end].join("\n").chars().take(8_000).collect())
}

fn line_column(root: &Path, path: &str, line: usize, needle: &str) -> Option<u64> {
    let content = std::fs::read_to_string(root.join(path)).ok()?;
    let text = content.lines().nth(line.saturating_sub(1))?;
    let byte = text.find(needle)?;
    Some((text[..byte].chars().count() as u64).saturating_add(1))
}

fn lexical_code_symbol(root: &Path, symbol: &Symbol, score: f64) -> CodeSymbol {
    let column = line_column(root, &symbol.path, symbol.line, &symbol.name).unwrap_or(1);
    let end_column = column.saturating_add(symbol.name.chars().count() as u64);
    let line = symbol.line as u64;
    let end_line = (symbol.end_line as u64).max(line);
    CodeSymbol {
        id: symbol.id.clone(),
        name: symbol.name.clone(),
        kind: symbol.kind.clone(),
        kind_group: kind_group(&symbol.kind).to_string(),
        language: symbol.language.clone(),
        path: symbol.path.clone(),
        line,
        column,
        end_line,
        end_column,
        range: CodeRange {
            line,
            column,
            end_line,
            end_column,
        },
        container: symbol.container.clone(),
        visibility: symbol.visibility.clone(),
        signature: symbol.signature.clone(),
        score,
        source: "lexical".to_string(),
        confidence: "lexical".to_string(),
        confidence_score: LEXICAL_CONFIDENCE_SYMBOL,
    }
}

fn parse_source_choice(
    input: &Value,
    default: SourceChoice,
) -> std::result::Result<SourceChoice, Box<ToolOutput>> {
    match input.get("source").and_then(Value::as_str) {
        Some(value) => SourceChoice::parse(value).ok_or_else(|| {
            Box::new(ToolOutput::error_structured(
                format!("invalid source: {value}"),
                json!({
                    "code": "INVALID_ARGUMENT",
                    "field": "source",
                    "allowed": ["auto", "lsp", "lexical", "build"],
                }),
            ))
        }),
        None => Ok(default),
    }
}

fn lsp_unavailable_output(error: &LspAttemptError, action: &str) -> ToolOutput {
    let mut structured = json!({
        "action": action,
        "code": error.code(),
        "status": error.status().as_str(),
        "source": "lsp",
        "confidence": "lsp",
        "confidence_score": 0.0,
        "message": error.to_string(),
        "fallback_reason": error.reason(),
        "searched": error.searched(),
        "supported_languages": lsp::all_languages().iter().map(|language| language.id).collect::<Vec<_>>(),
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

fn backend_attempts(error: &LspAttemptError) -> Vec<crate::tooling::code_intel::BackendAttempt> {
    vec![error.attempt()]
}

fn lexical_fallback_backend(error: &LspAttemptError) -> Backend {
    Backend::lexical(Status::Fallback, LEXICAL_CONFIDENCE_SYMBOL)
        .with_fallback(error.reason(), error.install_hint())
        .with_attempts(backend_attempts(error))
}

fn lsp_attempts(items: &[lsp::LspAttempt]) -> Vec<crate::tooling::code_intel::BackendAttempt> {
    items
        .iter()
        .map(lsp::LspAttempt::to_backend_attempt)
        .collect()
}

fn backend_for_lsp_batch(
    source: SourceChoice,
    status: Status,
    score: f64,
    attempts: &[lsp::LspAttempt],
) -> Backend {
    let primary = attempts
        .iter()
        .find(|attempt| attempt.status.is_success())
        .or_else(|| attempts.first());
    let mut backend = Backend::lsp(
        primary
            .map(|attempt| attempt.language.as_str())
            .unwrap_or("unknown"),
        primary.and_then(|attempt| attempt.server.as_deref()),
        primary
            .and_then(|attempt| attempt.method.as_deref())
            .unwrap_or("lsp"),
        status,
    )
    .with_score(score)
    .with_attempts(lsp_attempts(attempts));
    if let Some(version) = primary.and_then(|attempt| attempt.server_version.as_deref()) {
        backend = backend.with_server_version(version);
    }
    if source != SourceChoice::Lsp
        && let Some(attempt) = primary
    {
        backend = backend.with_language(display_language(&attempt.language));
    }
    backend
}

fn symbol_filter(
    symbol: &CodeSymbol,
    kind: Option<&str>,
    language: Option<&str>,
    path: Option<&str>,
) -> bool {
    if let Some(kind) = kind
        && symbol.kind != kind
        && symbol.kind_group != kind
    {
        return false;
    }
    if let Some(language) = language
        && !symbol.language.eq_ignore_ascii_case(language)
    {
        return false;
    }
    if let Some(path) = path
        && !symbol.path.starts_with(path)
    {
        return false;
    }
    true
}

fn symbol_values(root: &Path, symbols: &[CodeSymbol], include_body: bool) -> Vec<Value> {
    symbols
        .iter()
        .map(|symbol| {
            let mut value = serde_json::to_value(symbol).unwrap_or_else(|_| json!({}));
            if include_body
                && let Some(body) = read_code_body(root, &symbol.path, symbol.line, symbol.end_line)
            {
                value["body"] = json!(body);
            }
            value
        })
        .collect()
}

fn render_symbols_with_body(root: &Path, symbols: &[CodeSymbol], include_body: bool) -> String {
    if !include_body {
        return render_symbols(symbols);
    }
    symbols
        .iter()
        .map(|symbol| {
            let mut rendered = render_symbols(std::slice::from_ref(symbol));
            if let Some(body) = read_code_body(root, &symbol.path, symbol.line, symbol.end_line) {
                rendered.push('\n');
                rendered.push_str(&body);
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn symbol_heading(backend: &Backend, query: &str, count: usize) -> String {
    format!(
        "{} · {count} match{} for {query}",
        backend_sentence(backend),
        if count == 1 { "" } else { "es" }
    )
}

fn reference_values(locations: &[CodeLocation]) -> Vec<Value> {
    locations
        .iter()
        .map(|location| serde_json::to_value(location).unwrap_or_else(|_| json!({})))
        .collect()
}

fn lexical_reference_locations(
    root: &Path,
    index: &CodeIndex,
    symbol: &Symbol,
    limit: usize,
) -> (Vec<CodeLocation>, bool, usize) {
    let regex = match Regex::new(&format!(r"\b{}\b", regex::escape(&symbol.name))) {
        Ok(regex) => regex,
        Err(_) => return (Vec::new(), false, 0),
    };
    let mut references = Vec::new();
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    let mut scan_truncated = false;
    for file in &index.files {
        if file.bytes > MAX_INDEX_FILE_BYTES {
            continue;
        }
        if scanned_files >= MAX_SCAN_FILES || scanned_bytes >= MAX_SCAN_BYTES {
            scan_truncated = true;
            break;
        }
        scanned_files += 1;
        scanned_bytes = scanned_bytes.saturating_add(file.bytes);
        let Ok(content) = std::fs::read_to_string(root.join(&file.path)) else {
            continue;
        };
        for (line_index, text) in content.lines().enumerate() {
            for found in regex.find_iter(text) {
                let column = (text[..found.start()].chars().count() as u64).saturating_add(1);
                let end_column = (text[..found.end()].chars().count() as u64).saturating_add(1);
                let line = (line_index as u64).saturating_add(1);
                references.push(CodeLocation {
                    path: file.path.clone(),
                    line,
                    column,
                    end_line: line,
                    end_column,
                    range: CodeRange {
                        line,
                        column,
                        end_line: line,
                        end_column,
                    },
                    context: Some(text.trim().chars().take(240).collect()),
                    is_definition: file.path == symbol.path && line == symbol.line as u64,
                    kind: Some(symbol.kind.clone()),
                    source: "lexical".to_string(),
                    confidence: "lexical".to_string(),
                    confidence_score: LEXICAL_CONFIDENCE_REFERENCE,
                });
                if references.len() >= limit {
                    break;
                }
            }
            if references.len() >= limit {
                break;
            }
        }
        if references.len() >= limit {
            break;
        }
    }
    let (locations, capped) = prepare_locations(references, limit);
    (locations, scan_truncated || capped, scanned_files)
}

#[async_trait]
impl AgentTool for FindReferences {
    fn name(&self) -> &'static str {
        "find_references"
    }

    fn description(&self) -> &'static str {
        "Find exact language-server references when available, with a clearly labeled lexical fallback. Results include file, range, definition marker, symbol kind, source, and confidence."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "symbol_id": {"type": "string", "description": "Exact symbol id from find_symbol or repo_map"},
                "name": {"type": "string", "description": "Symbol name; required when symbol_id is absent"},
                "path": {"type": "string", "description": "Optional declaration path for disambiguation"},
                "line": {"type": "integer", "minimum": 1, "description": "Optional declaration line"},
                "column": {"type": "integer", "minimum": 1, "description": "Optional declaration character column"},
                "include_declaration": {"type": "boolean", "description": "Include the declaration; default true"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Default 100"},
                "source": {"type": "string", "enum": ["auto", "lsp", "lexical"], "description": "Default auto; lexical fallback is explicit"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let index = cached_index(&context.root);
        let source = match parse_source_choice(&input, SourceChoice::Auto) {
            Ok(source) => source,
            Err(output) => return Ok(*output),
        };
        if source == SourceChoice::Build {
            return Ok(ToolOutput::error_structured(
                "find_references does not have a build backend; use auto, lsp, or lexical",
                json!({"code": "INVALID_ARGUMENT", "field": "source"}),
            ));
        }
        let symbol_id = input.get("symbol_id").and_then(Value::as_str);
        let name = input.get("name").and_then(Value::as_str);
        let path = input.get("path").and_then(Value::as_str);
        let line = input.get("line").and_then(Value::as_u64);
        let column_input = input.get("column").and_then(Value::as_u64);
        let include_declaration = input
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, MAX_REFERENCES as u64) as usize;
        let symbol = resolve_reference_symbol(&index, symbol_id, name, path, line);
        let target = if let Some(symbol) = &symbol {
            let column = column_input
                .or_else(|| line_column(&context.root, &symbol.path, symbol.line, &symbol.name))
                .unwrap_or(1);
            Some((
                symbol.path.clone(),
                symbol.line as u64,
                column,
                symbol.name.clone(),
                Some(symbol.kind.clone()),
            ))
        } else if let Some(parsed) = symbol_id.and_then(parse_symbol_id) {
            let column = column_input
                .or_else(|| {
                    line_column(
                        &context.root,
                        &parsed.0,
                        usize::try_from(parsed.1).unwrap_or(usize::MAX),
                        &parsed.2,
                    )
                })
                .unwrap_or(1);
            Some((parsed.0, parsed.1, column, parsed.2, None))
        } else {
            None
        };
        let Some((
            declaration_path,
            declaration_line,
            declaration_column,
            symbol_name,
            symbol_kind,
        )) = target
        else {
            return Ok(ToolOutput::error_structured(
                "symbol_id or name is required and the declaration must be resolvable",
                json!({"code": "SYMBOL_NOT_FOUND", "name": name, "symbol_id": symbol_id}),
            ));
        };
        if source != SourceChoice::Lexical {
            let declaration = DeclarationPosition {
                path: context.root.join(&declaration_path),
                line: declaration_line,
                column: declaration_column,
                kind: symbol_kind.clone(),
            };
            match lsp::find_references(context, &declaration, include_declaration, limit).await {
                Ok(batch) => {
                    let locations = if include_declaration {
                        batch.locations
                    } else {
                        batch
                            .locations
                            .into_iter()
                            .filter(|location| !location.is_definition)
                            .collect()
                    };
                    let backend = backend_for_lsp_batch(
                        SourceChoice::Lsp,
                        batch.status,
                        LEXICAL_CONFIDENCE_REFERENCE,
                        &batch.attempts,
                    );
                    let values = reference_values(&locations);
                    let rendered = render_locations(&locations);
                    let heading = format!(
                        "{} · {} reference{} for {}",
                        backend_sentence(&backend),
                        locations.len(),
                        if locations.len() == 1 { "" } else { "s" },
                        symbol_name
                    );
                    let text = if rendered.is_empty() {
                        format!("{heading}\nno references")
                    } else {
                        format!("{heading}\n{rendered}")
                    };
                    let mut structured = json!({
                        "symbol": {
                            "name": symbol_name,
                            "path": declaration_path,
                            "line": declaration_line,
                            "column": declaration_column,
                            "kind": symbol_kind,
                        },
                        "declaration": batch.declaration,
                        "references": values,
                        "count": locations.len(),
                        "truncated": batch.truncated,
                    });
                    backend.apply(&mut structured);
                    return Ok(ToolOutput::ok_structured(truncate(text), structured));
                }
                Err(error) => {
                    if source == SourceChoice::Lsp {
                        return Ok(lsp_unavailable_output(&error, "find_references"));
                    }
                    let backend = lexical_fallback_backend(&error);
                    return Ok(lexical_references_output(
                        context,
                        &index,
                        symbol,
                        symbol_id,
                        name,
                        declaration_path,
                        declaration_line,
                        declaration_column,
                        symbol_name,
                        symbol_kind,
                        include_declaration,
                        limit,
                        backend,
                    ));
                }
            }
        }
        let backend = Backend::lexical(Status::Ok, LEXICAL_CONFIDENCE_REFERENCE);
        Ok(lexical_references_output(
            context,
            &index,
            symbol,
            symbol_id,
            name,
            declaration_path,
            declaration_line,
            declaration_column,
            symbol_name,
            symbol_kind,
            include_declaration,
            limit,
            backend,
        ))
    }
}

fn parse_symbol_id(symbol_id: &str) -> Option<(String, u64, String)> {
    let mut parts = symbol_id.rsplitn(3, ':');
    let name = parts.next()?.to_string();
    let line = parts.next()?.parse::<u64>().ok()?;
    let path = parts.next()?.to_string();
    if path.is_empty() || name.is_empty() || line == 0 {
        return None;
    }
    Some((path, line, name))
}

fn resolve_reference_symbol<'a>(
    index: &'a CodeIndex,
    symbol_id: Option<&str>,
    name: Option<&str>,
    path: Option<&str>,
    line: Option<u64>,
) -> Option<&'a Symbol> {
    if let Some(symbol_id) = symbol_id
        && let Some(symbol) = index.symbols.iter().find(|symbol| symbol.id == symbol_id)
    {
        return Some(symbol);
    }
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        return index
            .symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .min_by_key(|symbol| {
                let path_penalty = path.is_some_and(|path| symbol.path != path);
                let line_penalty = line.is_some_and(|line| symbol.line != line as usize);
                (path_penalty, line_penalty, symbol.line)
            });
    }
    let (path, line, name) = symbol_id.and_then(parse_symbol_id)?;
    index
        .symbols
        .iter()
        .filter(|symbol| symbol.name == name)
        .min_by_key(|symbol| {
            let path_penalty = symbol.path != path;
            let line_penalty = symbol.line != line as usize;
            (path_penalty, line_penalty, symbol.line)
        })
}

#[allow(clippy::too_many_arguments)]
fn lexical_references_output(
    context: &ToolContext,
    index: &CodeIndex,
    symbol: Option<&Symbol>,
    symbol_id: Option<&str>,
    _name: Option<&str>,
    declaration_path: String,
    declaration_line: u64,
    declaration_column: u64,
    symbol_name: String,
    symbol_kind: Option<String>,
    include_declaration: bool,
    limit: usize,
    backend: Backend,
) -> ToolOutput {
    let Some(symbol) = symbol else {
        return ToolOutput::error_structured(
            "symbol was not found in the lexical index; use source=lsp with an exact symbol_id or name/path/line",
            json!({
                "code": "SYMBOL_NOT_FOUND",
                "symbol_id": symbol_id,
                "path": declaration_path,
                "line": declaration_line,
            }),
        );
    };
    let (mut locations, scan_truncated, scanned_files) =
        lexical_reference_locations(&context.root, index, symbol, limit);
    if !include_declaration {
        locations.retain(|location| !location.is_definition);
    }
    for location in &mut locations {
        location.kind = symbol_kind.clone();
    }
    let values = reference_values(&locations);
    let rendered = render_locations(&locations);
    let heading = format!(
        "{} · {} reference{} for {}",
        backend_sentence(&backend),
        locations.len(),
        if locations.len() == 1 { "" } else { "s" },
        symbol_name
    );
    let text = if rendered.is_empty() {
        format!("{heading}\nno references")
    } else {
        format!("{heading}\n{rendered}")
    };
    let mut structured = json!({
        "symbol": {
            "name": symbol_name,
            "path": symbol.path,
            "line": declaration_line,
            "column": declaration_column,
            "kind": symbol_kind,
        },
        "references": values,
        "count": locations.len(),
        "truncated": scan_truncated || locations.len() >= limit,
        "scanned_files": scanned_files,
    });
    backend.apply(&mut structured);
    ToolOutput::ok_structured(truncate(text), structured)
}

#[async_trait]
impl AgentTool for CodeDiagnostics {
    fn name(&self) -> &'static str {
        "code_diagnostics"
    }

    fn description(&self) -> &'static str {
        "Return diagnostics grouped by file and severity. source=auto asks an installed language server first and falls back to the detected check/lint command with an explicit label; source=lsp never silently falls back."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Optional file or directory filters"},
                "severity": {"type": "string", "enum": ["error", "warning", "all"], "description": "Default error"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Default 100"},
                "source": {"type": "string", "enum": ["auto", "build", "lsp"], "description": "Default auto; lsp is exact-or-error"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let source = match parse_source_choice(&input, SourceChoice::Auto) {
            Ok(source) => source,
            Err(output) => return Ok(*output),
        };
        let severity = input
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("error");
        if !matches!(severity, "error" | "warning" | "all") {
            return Ok(ToolOutput::error_structured(
                format!("invalid severity: {severity}"),
                json!({"code": "INVALID_ARGUMENT", "field": "severity"}),
            ));
        }
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 200) as usize;
        let paths = input.get("paths").and_then(Value::as_array);
        if source == SourceChoice::Build {
            return build_diagnostics_output(context, severity, limit, paths, None).await;
        }
        let index = cached_index(&context.root);
        let files = match diagnostic_files(context, &index, paths) {
            Ok(files) => files,
            Err(output) => return Ok(*output),
        };
        if files.is_empty() {
            let error = LspAttemptError::NoLanguage {
                supported: lsp::all_languages()
                    .iter()
                    .map(|language| language.id.to_string())
                    .collect(),
            };
            if source == SourceChoice::Lsp {
                return Ok(lsp_unavailable_output(&error, "code_diagnostics"));
            }
            return build_diagnostics_output(context, severity, limit, paths, Some(&error)).await;
        }
        match lsp::collect_diagnostics(context, &files, limit).await {
            Ok(batch) => {
                let diagnostics = filter_diagnostic_severity(batch.diagnostics, severity);
                let summary = prepare_diagnostics(diagnostics, limit);
                let backend = backend_for_lsp_batch(
                    SourceChoice::Lsp,
                    batch.status,
                    BUILD_CONFIDENCE,
                    &batch.attempts,
                );
                let rendered = render_diagnostics(&summary);
                let heading = format!(
                    "{} · {} error{} · {} warning{}",
                    backend_sentence(&backend),
                    summary.errors,
                    if summary.errors == 1 { "" } else { "s" },
                    summary.warnings,
                    if summary.warnings == 1 { "" } else { "s" }
                );
                let text = if summary.total == 0 {
                    format!("{heading}\nno diagnostics")
                } else {
                    format!("{heading}\n{rendered}")
                };
                let group_values = serde_json::to_value(&summary.groups).unwrap_or(json!([]));
                let values = serde_json::to_value(
                    summary
                        .groups
                        .iter()
                        .flat_map(|group| group.diagnostics.clone())
                        .collect::<Vec<_>>(),
                )
                .unwrap_or(json!([]));
                let primary = batch
                    .attempts
                    .iter()
                    .find(|attempt| attempt.status.is_success())
                    .or_else(|| batch.attempts.first());
                let language = primary.map(|attempt| attempt.language.clone());
                let server = primary.and_then(|attempt| attempt.server.clone());
                let mut structured = json!({
                    "ok": summary.ok(),
                    "diagnostics": values,
                    "groups": group_values,
                    "group_count": summary.groups.len(),
                    "error_count": summary.errors,
                    "warning_count": summary.warnings,
                    "information_count": summary.information,
                    "hint_count": summary.hints,
                    "truncated": batch.truncated || summary.truncated,
                    "language": language,
                    "language_server": server,
                    "lsp_attempts": lsp_attempts(&batch.attempts),
                    "paths": paths.cloned().unwrap_or_default(),
                });
                backend.apply(&mut structured);
                Ok(ToolOutput::ok_structured(truncate(text), structured))
            }
            Err(error) => {
                if source == SourceChoice::Lsp {
                    return Ok(lsp_unavailable_output(&error, "code_diagnostics"));
                }
                build_diagnostics_output(context, severity, limit, paths, Some(&error)).await
            }
        }
    }
}

fn diagnostic_files(
    context: &ToolContext,
    index: &CodeIndex,
    paths: Option<&Vec<Value>>,
) -> std::result::Result<Vec<PathBuf>, Box<ToolOutput>> {
    let mut files = Vec::new();
    if let Some(paths) = paths {
        for raw in paths.iter().filter_map(Value::as_str) {
            let path = match safe_existing_path(&context.root, raw) {
                Ok(path) => path,
                Err(error) => {
                    return Err(Box::new(ToolOutput::error_structured(
                        format!("invalid diagnostics path {raw}: {error}"),
                        json!({"code": "INVALID_ARGUMENT", "path": raw}),
                    )));
                }
            };
            if path.is_file() {
                if lsp::language_for_path(&path).is_some() {
                    files.push(path);
                }
                continue;
            }
            let relative = path
                .strip_prefix(&context.root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let relative = relative.trim_start_matches("./");
            for file in &index.files {
                if file.path.starts_with(relative) {
                    let candidate = context.root.join(&file.path);
                    if lsp::language_for_path(&candidate).is_some() {
                        files.push(candidate);
                    }
                    if files.len() >= MAX_DIAGNOSTIC_FILES {
                        break;
                    }
                }
            }
            if files.len() >= MAX_DIAGNOSTIC_FILES {
                break;
            }
        }
    } else {
        for file in &index.files {
            let candidate = context.root.join(&file.path);
            if lsp::language_for_path(&candidate).is_some() {
                files.push(candidate);
            }
            if files.len() >= MAX_DIAGNOSTIC_FILES {
                break;
            }
        }
    }
    files.sort();
    files.dedup();
    files.truncate(MAX_DIAGNOSTIC_FILES);
    Ok(files)
}

fn filter_diagnostic_severity(
    diagnostics: Vec<CodeDiagnostic>,
    severity: &str,
) -> Vec<CodeDiagnostic> {
    diagnostics
        .into_iter()
        .filter(|diagnostic| match severity {
            "all" => true,
            "warning" => diagnostic.severity == "warning",
            _ => diagnostic.severity != "warning",
        })
        .collect()
}

fn build_diagnostic(
    failure: &crate::tooling::project::Failure,
    command_source: &str,
) -> CodeDiagnostic {
    let line = failure.line.unwrap_or(1).max(1) as u64;
    let column = failure.column.unwrap_or(1).max(1) as u64;
    CodeDiagnostic {
        path: failure
            .path
            .clone()
            .unwrap_or_else(|| "(project)".to_string()),
        line,
        column,
        end_line: line,
        end_column: column.saturating_add(1),
        range: CodeRange {
            line,
            column,
            end_line: line,
            end_column: column.saturating_add(1),
        },
        severity: if failure.severity == "warning" {
            "warning".to_string()
        } else {
            "error".to_string()
        },
        code: failure.code.clone().map(Value::String),
        source: Some(command_source.to_string()),
        server: None,
        message: failure.message.clone(),
        backend: "build".to_string(),
        confidence_score: BUILD_CONFIDENCE,
    }
}

async fn build_diagnostics_output(
    context: &ToolContext,
    severity: &str,
    limit: usize,
    paths: Option<&Vec<Value>>,
    fallback: Option<&LspAttemptError>,
) -> Result<ToolOutput> {
    let mut run = run_detected_task(context, "check", 300).await?;
    if run.is_none() {
        run = run_detected_task(context, "lint", 300).await?;
    }
    let Some(run) = run else {
        let profile = detect_profile(&context.root);
        let mut structured = json!({
            "code": "DIAGNOSTICS_UNAVAILABLE",
            "message": "no check or lint command was detected",
            "detected_commands": profile.commands.keys().collect::<Vec<_>>(),
            "source": "build",
            "status": "unavailable",
            "confidence": "compiler-or-parser",
            "confidence_score": 0.0,
        });
        if let Some(error) = fallback {
            structured["fallback_reason"] = json!(error.reason());
            structured["lsp_message"] = json!(error.to_string());
            if let Some(hint) = error.install_hint() {
                structured["install_hint"] = json!(hint);
            }
        }
        return Ok(ToolOutput::error_structured(
            "no language server and no check/lint command was detected",
            structured,
        ));
    };
    let diagnostics = run
        .failures
        .iter()
        .filter(|failure| {
            severity == "all"
                || (severity == "warning" && failure.severity == "warning")
                || (severity == "error" && failure.severity != "warning")
        })
        .filter(|failure| {
            paths.is_none_or(|paths| {
                failure.path.as_deref().is_some_and(|path| {
                    paths
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|filter| path.starts_with(filter.trim_start_matches("./")))
                })
            })
        })
        .map(|failure| build_diagnostic(failure, &run.source))
        .collect::<Vec<_>>();
    let summary = prepare_diagnostics(diagnostics, limit);
    let ok = run.exit_code == 0 && summary.errors == 0;
    let backend = Backend::build(if fallback.is_some() {
        Status::Fallback
    } else {
        Status::Ok
    })
    .with_method(run.command.clone())
    .with_fallback(
        fallback
            .map(LspAttemptError::reason)
            .unwrap_or("lsp_unavailable"),
        fallback.and_then(LspAttemptError::install_hint),
    )
    .with_attempts(fallback.map(backend_attempts).unwrap_or_default());
    let rendered = render_diagnostics(&summary);
    let heading = backend_sentence(&backend);
    let text = if ok && summary.total == 0 {
        format!("{heading}\nno diagnostics ({})", run.command)
    } else if summary.total == 0 {
        format!(
            "{heading}\n{} exited {} with no parsed diagnostics",
            run.command, run.exit_code
        )
    } else {
        format!("{heading}\n{rendered}")
    };
    let values = serde_json::to_value(
        summary
            .groups
            .iter()
            .flat_map(|group| group.diagnostics.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap_or(json!([]));
    let group_values = serde_json::to_value(&summary.groups).unwrap_or(json!([]));
    let mut structured = json!({
        "ok": ok,
        "command": run.command,
        "command_source": run.source,
        "exit_code": run.exit_code,
        "diagnostics": values,
        "groups": group_values,
        "group_count": summary.groups.len(),
        "error_count": summary.errors,
        "warning_count": summary.warnings,
        "parsed_warning_count": run.warning_count,
        "information_count": summary.information,
        "hint_count": summary.hints,
        "duration_ms": run.duration_ms,
        "raw_tail": tail_chars(&run.output, 4_000),
        "truncated": summary.truncated,
        "paths": paths.cloned().unwrap_or_default(),
    });
    backend.apply(&mut structured);
    Ok(ToolOutput::ok_structured(truncate(text), structured))
}

#[async_trait]
impl AgentTool for AffectedTests {
    fn name(&self) -> &'static str {
        "affected_tests"
    }

    fn description(&self) -> &'static str {
        "Map changed source paths or a Git base ref to likely test files and a focused test command."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "changed_paths": {"type": "array", "items": {"type": "string"}},
                "base_ref": {"type": "string", "description": "Git ref; uses git diff --name-only <ref>"},
                "mode": {"type": "string", "enum": ["lexical", "impact"], "description": "Mapping source; default lexical for compatibility"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Default 12"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let mode = match vp_protocol::test_cache::AffectedTestsMode::parse(
            input.get("mode").and_then(Value::as_str),
        ) {
            Ok(mode) => mode,
            Err(message) => {
                return Ok(ToolOutput::error_structured(
                    message,
                    json!({"code": "INVALID_ARGUMENT", "field": "mode"}),
                ));
            }
        };
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(12)
            .clamp(1, 50) as usize;
        let mut changed = input
            .get("changed_paths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if changed.is_empty()
            && let Some(base_ref) = input.get("base_ref").and_then(Value::as_str)
        {
            changed = git_diff_paths(&context.root, base_ref)?;
        }
        if changed.is_empty() {
            changed = git_changed_paths(&context.root);
        }
        if changed.is_empty() {
            return Ok(ToolOutput::error_structured(
                "no changed paths were provided or detected",
                json!({"code": "INVALID_ARGUMENT"}),
            ));
        }
        let profile = detect_profile(&context.root);
        let command = profile
            .commands
            .get("test")
            .map(|command| command.command.clone());
        let mut warnings = Vec::new();
        let (mut tests, impact) = if mode == vp_protocol::test_cache::AffectedTestsMode::Impact {
            match crate::test_cache::build_impact_map(
                context,
                context.test_cache.as_deref(),
                &changed,
                limit.max(1),
                true,
            )
            .await
            {
                Ok(map) => {
                    let mut tests = crate::test_cache::impact_targets_as_affected(&map);
                    tests.truncate(limit);
                    (tests, Some(map))
                }
                Err(error) => {
                    return Ok(ToolOutput::error_coded(
                        "affected_tests",
                        crate::tooling::errors::ToolErrorCode::IMPACT_MAP_UNAVAILABLE,
                        format!("impact mapping failed: {error}"),
                    )
                    .with_failure_evidence(format!("changed_paths={}", changed.len())));
                }
            }
        } else {
            let mut tests = affected_tests(&context.root, &changed);
            tests.truncate(limit);
            (tests, None)
        };
        tests.truncate(limit);
        if tests.is_empty() {
            warnings.push("no mapping found; run the full test suite");
        }
        let rendered = tests
            .iter()
            .map(|test| {
                format!(
                    "{} ({}, {:.0}%, {})",
                    test.path,
                    test.reason,
                    test.confidence * 100.0,
                    test.source
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut structured = json!({
            "changed_paths": changed,
            "tests": tests,
            "test_command": command,
            "warnings": warnings,
            "confidence": "lexical",
            "mode": mode.as_str(),
            "target_sources": tests.iter().map(|test| test.source.clone()).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
        });
        if let Some(map) = &impact {
            structured["confidence"] = json!(if map.lexical_only {
                "impact-lexical"
            } else {
                "impact"
            });
            structured["impact_generation"] = json!(map.generation);
            structured["profile_hash"] = json!(map.profile_hash);
            structured["sources"] = json!(map.sources);
            structured["dependency_paths"] = json!(map.dependency_paths);
            structured["lsp_available"] = json!(map.lsp_available);
            structured["lexical_only"] = json!(map.lexical_only);
            structured["truncated"] = json!(map.truncated);
            structured["warnings"] = json!(map.notes);
            structured["no_execution_note"] =
                json!("impact mapping never executes tests and does not claim completeness");
        }
        Ok(ToolOutput::ok_structured(
            truncate(if rendered.is_empty() {
                "no affected tests matched; run the full suite".to_string()
            } else {
                rendered
            }),
            structured,
        ))
    }
}

fn git_diff_paths(root: &Path, base_ref: &str) -> Result<Vec<String>> {
    if base_ref.trim().is_empty()
        || base_ref.starts_with('-')
        || base_ref
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || base_ref.contains("..")
    {
        bail!("invalid Git base ref");
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only", base_ref])
        .output()?;
    if !output.status.success() {
        bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .take(500)
        .map(str::to_string)
        .collect())
}

#[async_trait]
impl AgentTool for DependencyGraph {
    fn name(&self) -> &'static str {
        "dependency_graph"
    }

    fn description(&self) -> &'static str {
        "Show local import edges for a file: what it imports, what imports it, or both, with cycles and external imports."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the worktree root"},
                "direction": {"type": "string", "enum": ["imports", "imported_by", "both"], "description": "Default both"},
                "depth": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Default 2"},
                "include_external": {"type": "boolean", "description": "Default false"}
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let relative = input
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let target = safe_existing_path(&context.root, relative)?;
        if !target.is_file() {
            return Ok(ToolOutput::error_structured(
                "dependency_graph path must be a file",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let direction = input
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("both");
        let depth = input
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .clamp(1, 10) as usize;
        let include_external = input
            .get("include_external")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let index = cached_index(&context.root);
        let (graph, scan_truncated) = build_dependency_graph(&context.root, &index);
        let start = relative.trim_start_matches("./").to_string();
        if !graph.contains_key(&start) {
            return Ok(ToolOutput::error_structured(
                "file is not in the lexical index",
                json!({"code": "SYMBOL_NOT_FOUND", "path": start}),
            ));
        }
        let mut nodes = BTreeSet::new();
        let mut edges = Vec::new();
        let mut queue = VecDeque::from([(start.clone(), 0usize)]);
        let mut visited = HashSet::from([start.clone()]);
        while let Some((path, level)) = queue.pop_front() {
            nodes.insert(path.clone());
            if level >= depth {
                continue;
            }
            let links = match direction {
                "imports" => graph.get(&path).cloned().unwrap_or_default(),
                "imported_by" => graph
                    .iter()
                    .filter(|(_, imports)| imports.iter().any(|edge| edge.to == path))
                    .map(|(from, _)| ImportEdge {
                        from: from.clone(),
                        to: path.clone(),
                        external: false,
                        target: None,
                    })
                    .collect(),
                _ => {
                    let mut links = graph.get(&path).cloned().unwrap_or_default();
                    links.extend(
                        graph
                            .iter()
                            .filter(|(_, imports)| imports.iter().any(|edge| edge.to == path))
                            .map(|(from, _)| ImportEdge {
                                from: from.clone(),
                                to: path.clone(),
                                external: false,
                                target: None,
                            }),
                    );
                    links
                }
            };
            for edge in links {
                if edge.external && !include_external {
                    continue;
                }
                edges.push(edge.clone());
                if !edge.external && visited.insert(edge.to.clone()) {
                    queue.push_back((edge.to, level + 1));
                }
            }
            if nodes.len() >= MAX_GRAPH_NODES {
                break;
            }
        }
        edges.sort_by(|left, right| {
            left.from
                .cmp(&right.from)
                .then_with(|| left.to.cmp(&right.to))
        });
        edges.dedup_by(|left, right| {
            left.from == right.from && left.to == right.to && left.external == right.external
        });
        let cycles = find_cycles(&graph, &nodes);
        let rendered = edges
            .iter()
            .map(|edge| {
                if edge.external {
                    format!("{} -> external:{}", edge.from, edge.to)
                } else {
                    format!("{} -> {}", edge.from, edge.to)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok_structured(
            truncate(if rendered.is_empty() {
                "no matching import edges".to_string()
            } else {
                rendered
            }),
            json!({
                "root": start,
                "direction": direction,
                "nodes": nodes,
                "edges": edges,
                "cycles": cycles,
                "confidence": "lexical",
                "truncated": nodes.len() >= MAX_GRAPH_NODES || scan_truncated,
            }),
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
struct ImportEdge {
    from: String,
    to: String,
    external: bool,
    target: Option<String>,
}

fn build_dependency_graph(
    root: &Path,
    index: &CodeIndex,
) -> (BTreeMap<String, Vec<ImportEdge>>, bool) {
    let file_set = index
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let mut graph = BTreeMap::new();
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    let mut truncated = false;
    for file in &index.files {
        if file.is_config {
            continue;
        }
        if scanned_files >= MAX_SCAN_FILES || scanned_bytes >= MAX_SCAN_BYTES {
            truncated = true;
            break;
        }
        scanned_files += 1;
        scanned_bytes = scanned_bytes.saturating_add(file.bytes);
        let Ok(content) = std::fs::read_to_string(root.join(&file.path)) else {
            continue;
        };
        let mut edges = Vec::new();
        for import in parse_imports(&file.path, file.language, &content) {
            if let Some(resolved) = resolve_import(root, &file.path, &import.target)
                && file_set.contains(&resolved)
            {
                edges.push(ImportEdge {
                    from: file.path.clone(),
                    to: resolved,
                    external: false,
                    target: Some(import.target),
                });
                continue;
            }
            edges.push(ImportEdge {
                from: file.path.clone(),
                to: import.target.clone(),
                external: import.external,
                target: Some(import.target),
            });
        }
        graph.insert(file.path.clone(), edges);
    }
    (graph, truncated)
}

/// Reverse lexical dependency closure for deterministic impact mapping.
#[derive(Debug, Clone, Default)]
pub(crate) struct ImpactDependency {
    pub paths: Vec<String>,
    pub digest: String,
    pub truncated: bool,
}

/// Reference locations from an already-running language server. This is an
/// optional enrichment source; an empty result is expected when no server is
/// running and never blocks the lexical/profile mapping.
#[derive(Debug, Clone, Default)]
pub(crate) struct ImpactLspResult {
    pub available: bool,
    pub paths: Vec<String>,
    pub checked_symbols: usize,
    pub notes: Vec<String>,
}

/// Compute the bounded reverse import closure (files that may be affected by a
/// changed file) without reading test results or executing tests.
pub(crate) fn impact_dependency_closure(
    root: &Path,
    changed: &[String],
    max_depth: usize,
) -> ImpactDependency {
    const MAX_DEPENDENCY_PATHS: usize = 300;
    let index = cached_index(root);
    let (graph, scan_truncated) = build_dependency_graph(root, &index);
    let changed_set = changed
        .iter()
        .map(|path| path.trim_start_matches("./").to_string())
        .collect::<BTreeSet<_>>();
    if changed_set.is_empty() {
        return ImpactDependency::default();
    }
    let mut reverse: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, edges) in &graph {
        for edge in edges {
            if edge.external {
                continue;
            }
            reverse
                .entry(edge.to.as_str())
                .or_default()
                .push(from.as_str());
        }
    }
    let mut closure = BTreeSet::new();
    let mut queue = VecDeque::new();
    for path in &changed_set {
        queue.push_back((path.clone(), 0usize));
    }
    while let Some((path, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(importers) = reverse.get(path.as_str()) {
            for importer in importers {
                if changed_set.contains(*importer) || !closure.insert((*importer).to_string()) {
                    continue;
                }
                if closure.len() >= MAX_DEPENDENCY_PATHS {
                    break;
                }
                queue.push_back(((*importer).to_string(), depth + 1));
            }
        }
        if closure.len() >= MAX_DEPENDENCY_PATHS {
            break;
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"impact-dependency-v1");
    for path in &changed_set {
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
    }
    for path in &closure {
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
    }
    let mut edges = Vec::new();
    for (from, imports) in &graph {
        if !changed_set.contains(from) && !closure.contains(from) {
            continue;
        }
        for edge in imports {
            if !edge.external && (changed_set.contains(&edge.to) || closure.contains(&edge.to)) {
                edges.push(format!("{from}->{}", edge.to));
            }
        }
    }
    edges.sort();
    for edge in &edges {
        digest.update((edge.len() as u64).to_le_bytes());
        digest.update(edge.as_bytes());
    }
    let digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ImpactDependency {
        paths: closure.into_iter().collect(),
        digest,
        truncated: scan_truncated,
    }
}

/// Query references for up to four defined symbols per changed file using only
/// an already-running language server. No server is spawned and no test is
/// executed; an unavailable backend is reported as a note.
pub(crate) async fn impact_lsp_targets(
    context: &ToolContext,
    changed: &[String],
) -> ImpactLspResult {
    const MAX_SYMBOLS_PER_FILE: usize = 4;
    const MAX_REFERENCE_PATHS: usize = 80;
    let index = cached_index(&context.root);
    let mut result = ImpactLspResult::default();
    if index.truncated {
        result
            .notes
            .push("lexical index was truncated; LSP enrichment is partial".to_string());
    }
    const MAX_LSP_DEFINITIONS: usize = 24;
    let mut definitions = Vec::new();
    'paths: for path in changed {
        let normalized = path.trim_start_matches("./");
        let mut symbols = index
            .symbols
            .iter()
            .filter(|symbol| symbol.path == normalized)
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.name.cmp(&right.name))
        });
        for symbol in symbols.into_iter().take(MAX_SYMBOLS_PER_FILE) {
            if definitions.len() >= MAX_LSP_DEFINITIONS {
                result.notes.push(format!(
                    "LSP reference lookup was bounded at {MAX_LSP_DEFINITIONS} symbols"
                ));
                break 'paths;
            }
            definitions.push((normalized.to_string(), symbol.line, symbol.name.clone()));
        }
    }
    if definitions.is_empty() {
        result
            .notes
            .push("no lexical symbol definitions were found for LSP references".to_string());
        return result;
    }
    let mut paths = BTreeSet::new();
    for (path, line, name) in definitions {
        if paths.len() >= MAX_REFERENCE_PATHS {
            break;
        }
        let Some(column) = line_column(&context.root, &path, line, &name) else {
            continue;
        };
        let declaration = DeclarationPosition {
            path: context.root.join(&path),
            line: line as u64,
            column,
            kind: None,
        };
        match lsp::find_references_without_spawn(context, &declaration, false, 20).await {
            Ok(batch) => {
                result.available = true;
                result.checked_symbols += 1;
                for location in batch.locations {
                    if location.path.is_empty() || location.path == path {
                        continue;
                    }
                    paths.insert(location.path);
                    if paths.len() >= MAX_REFERENCE_PATHS {
                        break;
                    }
                }
                if batch.truncated {
                    result
                        .notes
                        .push("LSP reference results were truncated".to_string());
                }
            }
            Err(error) => {
                let note = format!("{}: {}", error.code(), error.reason());
                if !result.notes.contains(&note) {
                    result.notes.push(note);
                }
            }
        }
    }
    result.paths = paths.into_iter().collect();
    result
}

/// Existing profile candidates for a changed path, filtered by existence and
/// capped so one path never floods the impact map.
pub(crate) fn impact_profile_candidates(root: &Path, path: &str) -> Vec<(String, f64)> {
    profile_affected_candidates(root, path)
        .into_iter()
        .take(6)
        .collect()
}

struct ImportRef {
    target: String,
    external: bool,
}

fn parse_imports(path: &str, language: &str, content: &str) -> Vec<ImportRef> {
    let mut imports = Vec::new();
    match language {
        "Rust" => {
            for line in content.lines() {
                let line = line.trim();
                for prefix in ["use ", "pub use "] {
                    if let Some(rest) = line.strip_prefix(prefix) {
                        let target = rest
                            .trim_end_matches(';')
                            .split("::{")
                            .next()
                            .unwrap_or(rest)
                            .trim()
                            .to_string();
                        let external = !target.starts_with("crate::")
                            && !target.starts_with("self::")
                            && !target.starts_with("super::");
                        imports.push(ImportRef { target, external });
                    }
                }
                if let Some(rest) = line.strip_prefix("mod ")
                    && let Some(name) = rest.trim_end_matches(';').split_whitespace().next()
                {
                    imports.push(ImportRef {
                        target: format!("self::{name}"),
                        external: false,
                    });
                }
            }
        }
        "Python" => {
            for line in content.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("from ") {
                    if let Some((module, _)) = rest.split_once(" import ") {
                        imports.push(ImportRef {
                            target: module.trim().to_string(),
                            external: !module.trim().starts_with('.'),
                        });
                    }
                } else if let Some(rest) = line.strip_prefix("import ") {
                    for module in rest.split(',') {
                        imports.push(ImportRef {
                            target: module.trim().to_string(),
                            external: true,
                        });
                    }
                }
            }
        }
        "TypeScript" | "JavaScript" => {
            for line in content.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("import ") {
                    if let Some(module) = rest.split("from").nth(1) {
                        if let Some(target) = quoted(module) {
                            imports.push(ImportRef {
                                target: target.to_string(),
                                external: !target.starts_with('.'),
                            });
                        }
                    } else if let Some(target) = quoted(rest) {
                        imports.push(ImportRef {
                            target: target.to_string(),
                            external: !target.starts_with('.'),
                        });
                    }
                } else if let Some(position) = line.find("require(")
                    && let Some(target) = quoted(&line[position + 8..])
                {
                    imports.push(ImportRef {
                        target: target.to_string(),
                        external: !target.starts_with('.'),
                    });
                }
            }
        }
        "Go" => {
            let mut in_block = false;
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with("import (") {
                    in_block = true;
                    continue;
                }
                if in_block && line == ")" {
                    in_block = false;
                    continue;
                }
                let target = if in_block {
                    quoted(line).map(str::to_string)
                } else if let Some(rest) = line.strip_prefix("import ") {
                    quoted(rest).map(str::to_string)
                } else {
                    None
                };
                if let Some(target) = target {
                    let external = !target.contains(".");
                    imports.push(ImportRef { target, external });
                }
            }
        }
        "Swift" => {
            for line in content.lines() {
                if let Some(rest) = line.trim().strip_prefix("import ") {
                    imports.push(ImportRef {
                        target: rest.trim().to_string(),
                        external: true,
                    });
                }
            }
        }
        _ => {
            for line in content.lines() {
                if let Some(rest) = line.trim().strip_prefix("import ") {
                    imports.push(ImportRef {
                        target: rest.trim().to_string(),
                        external: true,
                    });
                }
            }
        }
    }
    let _ = path;
    imports
}

fn quoted(value: &str) -> Option<&str> {
    let start = value.find(['"', '\''])?;
    let quote = value.as_bytes()[start] as char;
    let rest = &value[start + 1..];
    let end = rest.find(quote)?;
    Some(&rest[..end])
}

fn resolve_import(root: &Path, from: &str, target: &str) -> Option<String> {
    let from_path = Path::new(from);
    let from_dir = from_path.parent().unwrap_or_else(|| Path::new(""));
    let candidates = if target.starts_with('.') {
        let base = from_dir.join(target);
        module_candidates(&base)
    } else if target.starts_with("crate::") {
        let rest = target.trim_start_matches("crate::").replace("::", "/");
        module_candidates(&PathBuf::from("src").join(rest))
    } else if target.starts_with("self::") {
        let rest = target.trim_start_matches("self::").replace("::", "/");
        module_candidates(&from_dir.join(rest))
    } else if target.starts_with("super::") {
        let rest = target.trim_start_matches("super::").replace("::", "/");
        module_candidates(&from_dir.join("..").join(rest))
    } else if target.contains("::") {
        let base = from_dir.join(target.replace("::", "/"));
        module_candidates(&base)
    } else if target.ends_with(".go") {
        vec![PathBuf::from(target)]
    } else if target.contains('/') && !target.starts_with('@') {
        module_candidates(&PathBuf::from(target))
    } else {
        vec![
            from_dir.join(format!("{target}.py")),
            from_dir.join(target).join("__init__.py"),
        ]
    };
    for candidate in candidates {
        let normalized = normalize_path(&candidate);
        if root.join(&normalized).is_file() {
            return Some(normalized);
        }
    }
    None
}

fn module_candidates(base: &Path) -> Vec<PathBuf> {
    let base = PathBuf::from(normalize_path(base));
    let mut candidates = vec![base.clone()];
    for extension in [
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "swift", "kt", "java",
    ] {
        candidates.push(PathBuf::from(format!(
            "{}.{}",
            base.to_string_lossy(),
            extension
        )));
    }
    for index in [
        "mod.rs",
        "index.ts",
        "index.tsx",
        "index.js",
        "index.jsx",
        "__init__.py",
    ] {
        candidates.push(base.join(index));
    }
    candidates
}

fn normalize_path(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

fn find_cycles(
    graph: &BTreeMap<String, Vec<ImportEdge>>,
    nodes: &BTreeSet<String>,
) -> Vec<Vec<String>> {
    let mut cycles = Vec::new();
    let mut stack = Vec::new();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for node in nodes {
        visit_cycles(
            node,
            graph,
            nodes,
            &mut stack,
            &mut visiting,
            &mut visited,
            &mut cycles,
        );
        if cycles.len() >= 10 {
            break;
        }
    }
    cycles
}

fn visit_cycles(
    node: &str,
    graph: &BTreeMap<String, Vec<ImportEdge>>,
    nodes: &BTreeSet<String>,
    stack: &mut Vec<String>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    if cycle_limit_reached(cycles) {
        return;
    }
    if !visiting.insert(node.to_string()) {
        if let Some(position) = stack.iter().position(|entry| entry == node) {
            cycles.push(stack[position..].to_vec());
        }
        return;
    }
    stack.push(node.to_string());
    for edge in graph.get(node).into_iter().flatten() {
        if !edge.external && nodes.contains(&edge.to) {
            visit_cycles(&edge.to, graph, nodes, stack, visiting, visited, cycles);
        }
        if cycle_limit_reached(cycles) {
            break;
        }
    }
    stack.pop();
    visiting.remove(node);
    visited.insert(node.to_string());
}

fn cycle_limit_reached(cycles: &[Vec<String>]) -> bool {
    cycles.len() >= 10
}

#[async_trait]
impl AgentTool for BuildContext {
    fn name(&self) -> &'static str {
        "build_context"
    }

    fn description(&self) -> &'static str {
        "Assemble a bounded, cited context packet for a goal from symbols, files, tests, project commands, and recent changes. No model call. Symbols are lexical by default and can optionally use an installed language server."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {"type": "string", "description": "What the agent is trying to do"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Optional focus paths"},
                "max_tokens": {"type": "integer", "minimum": 500, "maximum": 16000, "description": "Default 4000"},
                "include": {"type": "array", "items": {"type": "string", "enum": ["code", "tests", "git", "profile", "docs"]}, "description": "Sections to include"},
                "source": {"type": "string", "enum": ["auto", "lsp", "lexical"], "description": "Default lexical; auto only uses an already-running server"}
            },
            "required": ["goal"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let goal = input
            .get("goal")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
            .ok_or_else(|| anyhow!("goal is required"))?;
        let source = match parse_source_choice(&input, SourceChoice::Lexical) {
            Ok(source) => source,
            Err(output) => return Ok(*output),
        };
        if source == SourceChoice::Build {
            return Ok(ToolOutput::error_structured(
                "build_context does not have a build backend; use lexical or lsp",
                json!({"code": "INVALID_ARGUMENT", "field": "source"}),
            ));
        }
        let max_tokens = input
            .get("max_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(4_000)
            .clamp(500, MAX_CONTEXT_TOKENS as u64) as usize;
        let include = input
            .get("include")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_else(|| {
                ["code", "tests", "git", "profile"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            });
        let focus_paths = input
            .get("paths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tokens = tokens_for(goal);
        let index = cached_index(&context.root);
        let mut symbol_scores: Vec<(f64, &Symbol)> = index
            .symbols
            .iter()
            .filter(|symbol| {
                focus_paths.is_empty()
                    || focus_paths.iter().any(|path| symbol.path.starts_with(path))
            })
            .filter_map(|symbol| {
                let score = match_score(&symbol.name, &tokens, &symbol.path);
                (score > 0.0).then_some((score, symbol))
            })
            .collect();
        symbol_scores.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut selected_symbols = Vec::new();
        let mut file_scores: BTreeMap<String, f64> = BTreeMap::new();
        for (score, symbol) in symbol_scores.into_iter().take(30) {
            *file_scores.entry(symbol.path.clone()).or_default() += score;
            selected_symbols.push(lexical_code_symbol(&context.root, symbol, score));
            if selected_symbols.len() >= 12 {
                break;
            }
        }
        for file in &index.files {
            if !focus_paths.is_empty()
                && !focus_paths.iter().any(|path| file.path.starts_with(path))
            {
                continue;
            }
            let path_hit = match_score(&file.path, &tokens, "");
            if path_hit > 0.0 {
                *file_scores.entry(file.path.clone()).or_default() += path_hit * 0.25;
            }
        }
        let mut files = file_scores.into_iter().collect::<Vec<_>>();
        files.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });
        files.truncate(6);
        let mut backend = Backend::lexical(Status::Ok, LEXICAL_CONFIDENCE_SYMBOL);
        let mut symbol_source = "lexical";
        let mut lsp_attempt_list = Vec::new();
        if source != SourceChoice::Lexical && include.contains("code") {
            let lsp_files = files
                .iter()
                .map(|(path, _)| context.root.join(path))
                .collect::<Vec<_>>();
            let allow_spawn = source == SourceChoice::Lsp;
            match lsp::collect_file_symbols(context, &lsp_files, 24, allow_spawn).await {
                Ok(batch) => {
                    let mut enriched = batch
                        .symbols
                        .into_iter()
                        .filter_map(|symbol| {
                            let score = match_score(&symbol.name, &tokens, &symbol.path);
                            (score > 0.0).then_some((score, symbol))
                        })
                        .collect::<Vec<_>>();
                    enriched.sort_by(|left, right| {
                        right
                            .0
                            .partial_cmp(&left.0)
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then_with(|| left.1.path.cmp(&right.1.path))
                    });
                    lsp_attempt_list = lsp_attempts(&batch.attempts);
                    if !enriched.is_empty() {
                        selected_symbols = enriched
                            .into_iter()
                            .take(12)
                            .map(|(score, mut symbol)| {
                                symbol.score = score;
                                symbol
                            })
                            .collect();
                        symbol_source = "lsp";
                        backend = backend_for_lsp_batch(
                            SourceChoice::Lsp,
                            batch.status,
                            LSP_CONFIDENCE_SYMBOL,
                            &batch.attempts,
                        );
                    } else {
                        backend = Backend::lexical(Status::Fallback, LEXICAL_CONFIDENCE_SYMBOL)
                            .with_fallback("lsp_empty", None)
                            .with_attempts(lsp_attempt_list.clone());
                    }
                }
                Err(error) => {
                    if source == SourceChoice::Lsp {
                        return Ok(lsp_unavailable_output(&error, "build_context"));
                    }
                    backend = lexical_fallback_backend(&error);
                }
            }
        }
        let char_budget = max_tokens.saturating_mul(4);
        let mut sections = Vec::new();
        let mut citations = Vec::new();
        if include.contains("profile") {
            let profile = detect_profile(&context.root);
            let commands = profile
                .commands
                .iter()
                .map(|(kind, command)| format!("{kind}: {}", command.command))
                .collect::<Vec<_>>()
                .join("\n");
            if !commands.is_empty() {
                sections.push(format!("## Project commands\n{commands}"));
            }
            let conventions = profile
                .conventions
                .iter()
                .map(|convention| format!("{}: {}", convention.path, convention.summary))
                .collect::<Vec<_>>()
                .join("\n");
            if !conventions.is_empty() {
                sections.push(format!("## Conventions\n{conventions}"));
            }
        }
        if include.contains("code") && !selected_symbols.is_empty() {
            let symbols = selected_symbols
                .iter()
                .map(|symbol| {
                    citations.push(json!({
                        "path": symbol.path,
                        "line": symbol.line,
                        "reason": if symbol_source == "lsp" { "lsp symbol" } else { "lexical symbol" },
                    }));
                    format!(
                        "- {}:{}:{} {} {}",
                        symbol.path, symbol.line, symbol.column, symbol.kind, symbol.name
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("## Relevant symbols\n{symbols}"));
        }
        if include.contains("tests") {
            let changed = files
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>();
            let related: Vec<AffectedTest> = affected_tests(&context.root, &changed)
                .into_iter()
                .take(8)
                .collect();
            if !related.is_empty() {
                let tests = related
                    .iter()
                    .map(|test| format!("- {} ({})", test.path, test.reason))
                    .collect::<Vec<_>>()
                    .join("\n");
                sections.push(format!("## Related tests\n{tests}"));
            }
        }
        if include.contains("git") {
            let changes = git_summary(&context.root);
            if !changes.is_empty() {
                sections.push(format!("## Recent changes\n{changes}"));
            }
        }
        if include.contains("code") {
            for (path, score) in files {
                if sections.iter().map(String::len).sum::<usize>() >= char_budget {
                    break;
                }
                let Ok(content) = std::fs::read_to_string(context.root.join(&path)) else {
                    continue;
                };
                let lines = content.lines().collect::<Vec<_>>();
                let anchor = tokens
                    .iter()
                    .filter_map(|token| {
                        lines.iter().position(|line| {
                            line.to_ascii_lowercase()
                                .contains(&token.to_ascii_lowercase())
                        })
                    })
                    .min()
                    .unwrap_or(0);
                let start = anchor.saturating_sub(12);
                let end = (anchor + 40).min(lines.len());
                let snippet = lines[start..end].join("\n");
                citations.push(json!({
                    "path": path,
                    "line": start + 1,
                    "reason": format!("lexical score {score:.2}"),
                }));
                sections.push(format!(
                    "## File {path}\n```text\n{}:{}…\n{}\n```",
                    path,
                    start + 1,
                    snippet
                ));
            }
        }
        let mut packet = sections.join("\n\n");
        let truncated = packet.len() > char_budget;
        if truncated {
            let mut end = char_budget;
            while end > 0 && !packet.is_char_boundary(end) {
                end -= 1;
            }
            packet.truncate(end);
            packet.push_str("\n… context truncated");
        }
        let estimated_tokens = packet.len() / 4;
        let summary = format!(
            "{} chars · ~{} tokens · {} symbols · {} files · {}",
            packet.len(),
            estimated_tokens,
            selected_symbols.len(),
            files_seen(&citations),
            backend_sentence(&backend)
        );
        let mut structured = json!({
            "goal": goal,
            "context": packet,
            "citations": citations,
            "estimated_tokens": estimated_tokens,
            "max_tokens": max_tokens,
            "truncated": truncated,
            "symbol_source": symbol_source,
            "symbols": selected_symbols,
            "lsp_attempts": lsp_attempt_list,
            "next_actions": ["read the cited files before editing"],
        });
        backend.apply(&mut structured);
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n\n{packet}")),
            structured,
        ))
    }
}

fn tail_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let skip = text.chars().count() - limit;
    let tail = text.chars().skip(skip).collect::<String>();
    format!("… (earlier output omitted)\n{tail}")
}

fn files_seen(citations: &[Value]) -> usize {
    citations
        .iter()
        .filter_map(|citation| citation["path"].as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn git_summary(root: &Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--stat", "--no-color"])
        .output();
    let Ok(output) = output else {
        return String::new();
    };
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return String::new();
    }
    if text.len() > 2_000 {
        text.truncate(2_000);
        text.push_str("\n… stat truncated");
    }
    text
}

fn tokens_for(goal: &str) -> BTreeSet<String> {
    let stop = [
        "the",
        "a",
        "an",
        "to",
        "of",
        "and",
        "or",
        "in",
        "on",
        "for",
        "with",
        "this",
        "that",
        "is",
        "are",
        "be",
        "it",
        "as",
        "at",
        "by",
        "from",
        "into",
        "when",
        "then",
        "add",
        "fix",
        "make",
        "use",
        "using",
        "update",
        "implement",
        "create",
        "change",
        "please",
    ];
    let mut tokens = BTreeSet::new();
    for raw in goal.split(|ch: char| !ch.is_alphanumeric()) {
        if raw.len() < 2 {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if stop.contains(&lower.as_str()) {
            continue;
        }
        tokens.insert(lower);
        let mut current = String::new();
        let mut previous_lower = false;
        for ch in raw.chars() {
            if ch.is_uppercase() && previous_lower && !current.is_empty() {
                tokens.insert(current.to_ascii_lowercase());
                current.clear();
            }
            previous_lower = ch.is_lowercase() || ch.is_numeric();
            current.push(ch);
        }
        if current.len() > 1 {
            tokens.insert(current.to_ascii_lowercase());
        }
    }
    tokens
}

fn match_score(value: &str, tokens: &BTreeSet<String>, path: &str) -> f64 {
    let lower = value.to_ascii_lowercase();
    let path_lower = path.to_ascii_lowercase();
    let mut score = 0.0;
    for token in tokens {
        if lower == *token {
            score += 1.0;
        } else if lower.starts_with(token) {
            score += 0.8;
        } else if lower.contains(token) {
            score += 0.5;
        }
        if path_lower.contains(token) {
            score += 0.2;
        }
    }
    score
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
            test_cache: None,
        }
    }

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub struct Widget { value: u32 }\n\nimpl Widget {\n    pub fn new(value: u32) -> Self { Self { value } }\n}\n\npub fn build_widget() -> Widget {\n    Widget::new(1)\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "use crate::lib::Widget;\n\nfn main() {\n    let _ = Widget::new(1);\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/lib_test.rs"),
            "#[test]\nfn widget_works() {\n    let _ = crate::lib::build_widget();\n}\n",
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn builds_a_repo_map_and_finds_symbols() {
        let dir = fixture();
        let map = RepoMap
            .execute(&context(dir.path()), json!({"include_tests": true}))
            .await
            .unwrap();
        assert!(!map.is_error, "{}", map.content);
        assert!(map.content.contains("build_widget"));
        assert!(map.content.contains("Widget"));

        let found = FindSymbol
            .execute(&context(dir.path()), json!({"query": "build_widget"}))
            .await
            .unwrap();
        assert!(!found.is_error);
        assert!(found.content.contains("src/lib.rs"));

        let body = FindSymbol
            .execute(
                &context(dir.path()),
                json!({"query": "build_widget", "include_body": true}),
            )
            .await
            .unwrap();
        assert!(body.content.contains("Widget::new(1)"));
    }

    #[tokio::test]
    async fn finds_lexical_references() {
        let dir = fixture();
        let output = FindReferences
            .execute(
                &context(dir.path()),
                json!({"name": "Widget", "path": "src/lib.rs"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("src/main.rs"));
        assert!(output.content.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn builds_dependency_edges_and_context() {
        let dir = fixture();
        let graph = DependencyGraph
            .execute(
                &context(dir.path()),
                json!({"path": "src/main.rs", "direction": "imports"}),
            )
            .await
            .unwrap();
        assert!(!graph.is_error, "{}", graph.content);
        assert!(graph.content.contains("src/lib.rs") || graph.content.contains("lib"));

        let packet = BuildContext
            .execute(
                &context(dir.path()),
                json!({"goal": "change build_widget", "max_tokens": 1000}),
            )
            .await
            .unwrap();
        assert!(!packet.is_error, "{}", packet.content);
        assert!(packet.content.contains("build_widget"));
        assert!(packet.content.contains("cargo"));
    }

    #[test]
    fn extracts_python_and_go_symbols() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("a.py"),
            "class Thing:\n    def method(self):\n        pass\n\ndef helper():\n    pass\n",
        )
        .unwrap();
        let index = build_index(dir.path());
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.name == "Thing" && symbol.kind == "class")
        );
        assert!(
            index
                .symbols
                .iter()
                .any(|symbol| symbol.name == "helper" && symbol.kind == "function")
        );

        let go = TempDir::new().unwrap();
        std::fs::write(
            go.path().join("a.go"),
            "package main\n\ntype Thing struct{}\n\nfunc Build() {}\n",
        )
        .unwrap();
        let index = build_index(go.path());
        assert!(index.symbols.iter().any(|symbol| symbol.name == "Thing"));
        assert!(index.symbols.iter().any(|symbol| symbol.name == "Build"));
    }

    #[tokio::test]
    async fn lexical_symbol_results_declare_source_range_and_confidence() {
        let dir = fixture();
        let output = FindSymbol
            .execute(
                &context(dir.path()),
                json!({"query": "build_widget", "source": "lexical"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["status"], "ok");
        assert_eq!(structured["confidence"], "lexical");
        assert_eq!(structured["confidence_score"], 0.75);
        assert_eq!(structured["backend"]["source"], "lexical");
        let symbol = &structured["symbols"][0];
        assert_eq!(symbol["path"], "src/lib.rs");
        assert!(symbol["line"].as_u64().unwrap() >= 1);
        assert!(symbol["column"].as_u64().unwrap() >= 1);
        assert!(symbol["end_line"].as_u64().unwrap() >= symbol["line"].as_u64().unwrap());
        assert_eq!(symbol["range"]["line"], symbol["line"]);
        assert_eq!(symbol["range"]["end_line"], symbol["end_line"]);
        assert_eq!(symbol["kind"], "function");
        assert_eq!(symbol["kind_group"], "function");
        assert_eq!(symbol["source"], "lexical");
        assert_eq!(symbol["confidence"], "lexical");
        assert!(output.content.contains("lexical index"));
        assert!(output.content.contains("not compiler-accurate"));
    }

    #[tokio::test]
    async fn explicit_lsp_without_a_mapped_language_is_unavailable_not_fake_exact() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "no code here\n").unwrap();
        let output = FindSymbol
            .execute(
                &context(dir.path()),
                json!({"query": "Widget", "source": "lsp"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "LSP_UNAVAILABLE");
        assert_eq!(structured["status"], "unavailable");
        assert_eq!(structured["source"], "lsp");
        assert_eq!(structured["confidence_score"], 0.0);
        assert!(
            structured["supported_languages"]
                .as_array()
                .is_some_and(|languages| !languages.is_empty())
        );
    }

    #[tokio::test]
    async fn auto_symbol_search_labels_the_lexical_fallback() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "no code here\n").unwrap();
        let output = FindSymbol
            .execute(
                &context(dir.path()),
                json!({"query": "Widget", "source": "auto"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["status"], "fallback");
        assert_eq!(structured["fallback_reason"], "lsp_unsupported_language");
        assert_eq!(structured["backend"]["status"], "fallback");
        assert!(output.content.contains("lexical fallback"));
    }

    #[tokio::test]
    async fn lexical_references_include_range_column_and_definition_marker() {
        let dir = fixture();
        let output = FindReferences
            .execute(
                &context(dir.path()),
                json!({
                    "name": "Widget",
                    "path": "src/lib.rs",
                    "source": "lexical",
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["status"], "ok");
        assert_eq!(structured["confidence_score"], 0.55);
        assert_eq!(structured["symbol"]["name"], "Widget");
        assert_eq!(structured["symbol"]["path"], "src/lib.rs");
        let references = structured["references"].as_array().unwrap();
        assert!(!references.is_empty());
        assert!(
            references
                .iter()
                .any(|reference| reference["is_definition"] == true)
        );
        for reference in references {
            assert!(reference["path"].is_string());
            assert!(reference["line"].as_u64().unwrap() >= 1);
            assert!(reference["column"].as_u64().unwrap() >= 1);
            assert!(reference["range"]["end_line"].as_u64().unwrap() >= 1);
            assert_eq!(reference["kind"], "type");
            assert_eq!(reference["source"], "lexical");
            assert_eq!(reference["confidence_score"], 0.55);
        }
    }

    #[tokio::test]
    async fn lexical_references_include_multiple_matches_on_one_line() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "struct Widget;\nfn f() { let _ = Widget; let _ = Widget; }\n",
        )
        .unwrap();
        let output = FindReferences
            .execute(
                &context(dir.path()),
                json!({"name": "Widget", "source": "lexical"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        let references = structured["references"].as_array().unwrap();
        assert_eq!(references.len(), 3);
        let line_two = references
            .iter()
            .filter(|reference| reference["line"] == 2)
            .collect::<Vec<_>>();
        assert_eq!(line_two.len(), 2);
        assert!(line_two[0]["column"].as_u64().unwrap() < line_two[1]["column"].as_u64().unwrap());
    }

    #[tokio::test]
    async fn find_references_accepts_a_symbol_id_that_is_not_in_the_index() {
        let dir = fixture();
        let output = FindReferences
            .execute(
                &context(dir.path()),
                json!({
                    "symbol_id": "src/lib.rs:1:build_widget",
                    "source": "lexical",
                }),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["symbol"]["path"], "src/lib.rs");
        assert!(structured["symbol"]["line"].as_u64().unwrap() >= 1);
        assert_eq!(structured["symbol"]["name"], "build_widget");
    }

    #[tokio::test]
    async fn diagnostics_explicit_lsp_without_mapping_is_structured_unavailable() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "no code here\n").unwrap();
        let output = CodeDiagnostics
            .execute(&context(dir.path()), json!({"source": "lsp"}))
            .await
            .unwrap();
        assert!(output.is_error);
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "LSP_UNAVAILABLE");
        assert_eq!(structured["status"], "unavailable");
        assert_eq!(structured["source"], "lsp");
    }

    #[tokio::test]
    async fn diagnostics_auto_without_any_backend_reports_build_unavailable() {
        let dir = TempDir::new().unwrap();
        let output = CodeDiagnostics
            .execute(&context(dir.path()), json!({"source": "auto"}))
            .await
            .unwrap();
        assert!(output.is_error);
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "DIAGNOSTICS_UNAVAILABLE");
        assert_eq!(structured["source"], "build");
        assert_eq!(structured["status"], "unavailable");
        assert_eq!(structured["fallback_reason"], "lsp_unsupported_language");
        assert!(
            output.content.contains("DIAGNOSTICS_UNAVAILABLE"),
            "provider text must carry the stable error code: {}",
            output.content
        );
        assert!(
            output
                .content
                .contains("no check or lint command was detected")
        );
    }

    #[tokio::test]
    async fn repo_map_and_context_label_lexical_evidence() {
        let dir = fixture();
        let map = RepoMap
            .execute(
                &context(dir.path()),
                json!({"include_tests": true, "source": "lexical"}),
            )
            .await
            .unwrap();
        let structured = map.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["symbol_source"], "lexical");
        assert_eq!(structured["symbols"][0]["source"], "lexical");

        let packet = BuildContext
            .execute(
                &context(dir.path()),
                json!({"goal": "change build_widget", "source": "lexical"}),
            )
            .await
            .unwrap();
        let structured = packet.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["symbol_source"], "lexical");
        assert!(
            structured["citations"]
                .as_array()
                .is_some_and(|c| !c.is_empty())
        );
    }

    #[tokio::test]
    async fn repo_map_explicit_lsp_without_source_files_is_unavailable() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "no code here\n").unwrap();
        let output = RepoMap
            .execute(&context(dir.path()), json!({"source": "lsp"}))
            .await
            .unwrap();
        assert!(output.is_error);
        let structured = output.structured.unwrap();
        assert_eq!(structured["code"], "LSP_UNAVAILABLE");
        assert_eq!(structured["status"], "unavailable");
    }

    #[tokio::test]
    async fn repo_map_auto_without_a_running_server_labels_lexical_fallback() {
        let dir = fixture();
        let output = RepoMap
            .execute(
                &context(dir.path()),
                json!({"include_tests": true, "source": "auto"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["status"], "fallback");
        assert_eq!(structured["fallback_reason"], "lsp_unavailable");
        assert_eq!(structured["symbol_source"], "lexical");
        assert!(structured["install_hint"].as_str().is_some());
    }

    #[tokio::test]
    async fn build_context_auto_without_a_running_server_labels_lexical_fallback() {
        let dir = fixture();
        let output = BuildContext
            .execute(
                &context(dir.path()),
                json!({"goal": "change build_widget", "source": "auto"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        let structured = output.structured.unwrap();
        assert_eq!(structured["source"], "lexical");
        assert_eq!(structured["status"], "fallback");
        assert_eq!(structured["symbol_source"], "lexical");
    }

    #[test]
    fn parses_source_choices_and_symbol_ids() {
        assert_eq!(
            parse_source_choice(&json!({"source": "auto"}), SourceChoice::Lexical).unwrap(),
            SourceChoice::Auto
        );
        assert_eq!(
            parse_source_choice(&json!({"source": "LSP"}), SourceChoice::Lexical).unwrap(),
            SourceChoice::Lsp
        );
        assert!(parse_source_choice(&json!({"source": "model"}), SourceChoice::Lexical).is_err());
        assert_eq!(
            parse_symbol_id("src/lib.rs:12:Widget"),
            Some(("src/lib.rs".to_string(), 12, "Widget".to_string()))
        );
        assert_eq!(parse_symbol_id("Widget"), None);
        assert_eq!(parse_symbol_id("src/lib.rs:0:Widget"), None);
    }
}

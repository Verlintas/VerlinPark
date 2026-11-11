//! Deterministic code intelligence for agent tools.
//!
//! The implementation is a bounded lexical index, not a full language server.
//! It extracts symbols with per-language patterns and resolves imports where a
//! project layout makes that unambiguous. Every result reports lexical
//! confidence so the model does not confuse it with compiler-accurate
//! navigation. LSP remains an optional future backend.

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

use crate::tooling::project::{
    AffectedTest, affected_tests, detect_profile, git_changed_paths, run_detected_task,
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

fn is_test_path(path: &str) -> bool {
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
        "Return a compact map of repository languages, files, public symbols, tests, and config boundaries. Lexical confidence, not LSP."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory relative to the worktree root; default ."},
                "max_files": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "Default 2000"},
                "max_symbols": {"type": "integer", "minimum": 1, "maximum": 800, "description": "Default 300"},
                "include_tests": {"type": "boolean", "description": "Include test files and symbols; default false"}
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
            .collect::<Vec<_>>();
        symbols.sort_by(|left, right| {
            right
                .visibility
                .is_some()
                .cmp(&left.visibility.is_some())
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.line.cmp(&right.line))
        });
        let symbols = symbols
            .into_iter()
            .take(max_symbols)
            .cloned()
            .collect::<Vec<_>>();
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
        let rendered = symbols
            .iter()
            .map(|symbol| {
                format!(
                    "{}:{} · {} {}",
                    symbol.path, symbol.line, symbol.kind, symbol.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let language_names = index
            .languages
            .iter()
            .take(5)
            .map(|language| format!("{} ({})", language.name, language.files))
            .collect::<Vec<_>>()
            .join(", ");
        let summary = format!(
            "{} files · {} languages{} · {} symbols · {} tests",
            files.len(),
            index.languages.len(),
            if language_names.is_empty() {
                String::new()
            } else {
                format!(" ({language_names})")
            },
            symbols.len(),
            tests.len()
        );
        let structured = json!({
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
            "truncated": index.truncated || files.len() >= max_files,
            "confidence": "lexical",
            "build_ms": index.build_ms,
        });
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
        "Find a function, type, module, or other symbol by name. Returns path, line range, signature, and optional body."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Symbol name or substring"},
                "kind": {"type": "string", "description": "Optional kind filter such as function, type, module"},
                "language": {"type": "string", "description": "Optional language filter"},
                "path": {"type": "string", "description": "Optional directory filter"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Default 20"},
                "include_body": {"type": "boolean", "description": "Include the symbol body; default false"}
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
        let query_lower = query.to_ascii_lowercase();
        let mut matches = index
            .symbols
            .iter()
            .filter(|symbol| kind.is_none_or(|kind| symbol.kind == kind))
            .filter(|symbol| {
                language.is_none_or(|language| symbol.language.eq_ignore_ascii_case(language))
            })
            .filter(|symbol| path_filter.is_none_or(|path| symbol.path.starts_with(path)))
            .filter_map(|symbol| {
                let name = symbol.name.to_ascii_lowercase();
                let score = if symbol.name == query {
                    1.0
                } else if name == query_lower {
                    0.95
                } else if name.starts_with(&query_lower) {
                    0.9
                } else if name.contains(&query_lower) {
                    0.75
                } else if query
                    .split(|ch: char| !ch.is_alphanumeric())
                    .filter(|token| !token.is_empty())
                    .all(|token| name.contains(&token.to_ascii_lowercase()))
                {
                    0.65
                } else {
                    return None;
                };
                Some((score, symbol))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    right
                        .1
                        .visibility
                        .is_some()
                        .cmp(&left.1.visibility.is_some())
                })
                .then_with(|| left.1.path.cmp(&right.1.path))
                .then_with(|| left.1.line.cmp(&right.1.line))
        });
        matches.truncate(limit);
        let mut results = Vec::new();
        for (score, symbol) in matches {
            let mut value = serde_json::to_value(symbol).unwrap_or_else(|_| json!({}));
            value["score"] = json!(score);
            if include_body {
                let body = read_body(&context.root, symbol).unwrap_or_default();
                value["body"] = json!(body);
            }
            results.push(value);
        }
        let rendered = results
            .iter()
            .map(|value| {
                let mut line = format!(
                    "{}:{} · {} {} · {}",
                    value["path"].as_str().unwrap_or("?"),
                    value["line"].as_u64().unwrap_or(0),
                    value["kind"].as_str().unwrap_or("symbol"),
                    value["name"].as_str().unwrap_or("?"),
                    value["signature"].as_str().unwrap_or("")
                );
                if let Some(body) = value["body"].as_str() {
                    line.push('\n');
                    line.push_str(body);
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(ToolOutput::ok_structured(
            truncate(if rendered.is_empty() {
                format!("no symbols matched {query}")
            } else {
                rendered
            }),
            json!({
                "query": query,
                "symbols": results,
                "count": results.len(),
                "confidence": "lexical",
            }),
        ))
    }
}

fn read_body(root: &Path, symbol: &Symbol) -> Result<String> {
    let path = root.join(&symbol.path);
    let content = std::fs::read_to_string(&path)?;
    let lines = content.lines().collect::<Vec<_>>();
    let start = symbol.line.saturating_sub(1).min(lines.len());
    let end = symbol.end_line.min(lines.len());
    let body = lines[start..end].join("\n");
    Ok(body.chars().take(8_000).collect())
}

#[async_trait]
impl AgentTool for FindReferences {
    fn name(&self) -> &'static str {
        "find_references"
    }

    fn description(&self) -> &'static str {
        "Find textual references to a symbol. Results are lexical, not LSP-accurate; use file/line to disambiguate."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "symbol_id": {"type": "string", "description": "Exact symbol id from find_symbol or repo_map"},
                "name": {"type": "string", "description": "Symbol name; required when symbol_id is absent"},
                "path": {"type": "string", "description": "Optional declaration path for disambiguation"},
                "line": {"type": "integer", "minimum": 1, "description": "Optional declaration line"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Default 100"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let index = cached_index(&context.root);
        let symbol_id = input.get("symbol_id").and_then(Value::as_str);
        let name = input.get("name").and_then(Value::as_str);
        let path = input.get("path").and_then(Value::as_str);
        let line = input.get("line").and_then(Value::as_u64);
        let symbol = if let Some(symbol_id) = symbol_id {
            index.symbols.iter().find(|symbol| symbol.id == symbol_id)
        } else {
            let name = name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow!("symbol_id or name is required"))?;
            index
                .symbols
                .iter()
                .filter(|symbol| symbol.name == name)
                .min_by_key(|symbol| {
                    let path_penalty = path.is_some_and(|path| symbol.path != path);
                    let line_penalty = line.is_some_and(|line| symbol.line != line as usize);
                    (path_penalty, line_penalty, symbol.line)
                })
        };
        let Some(symbol) = symbol else {
            return Ok(ToolOutput::error_structured(
                "symbol was not found in the lexical index",
                json!({"code": "SYMBOL_NOT_FOUND", "name": name, "symbol_id": symbol_id}),
            ));
        };
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, MAX_REFERENCES as u64) as usize;
        let regex = Regex::new(&format!(r"\b{}\b", regex::escape(&symbol.name)))
            .map_err(|error| anyhow!("building reference regex: {error}"))?;
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
            let Ok(content) = std::fs::read_to_string(context.root.join(&file.path)) else {
                continue;
            };
            for (index, text) in content.lines().enumerate() {
                if !regex.is_match(text) {
                    continue;
                }
                let is_definition = file.path == symbol.path && index + 1 == symbol.line;
                references.push(json!({
                    "path": file.path,
                    "line": index + 1,
                    "text": text.trim().chars().take(240).collect::<String>(),
                    "is_definition": is_definition,
                    "confidence": "lexical",
                }));
                if references.len() >= limit {
                    break;
                }
            }
            if references.len() >= limit {
                break;
            }
        }
        let rendered = references
            .iter()
            .map(|reference| {
                format!(
                    "{}:{}: {}",
                    reference["path"].as_str().unwrap_or("?"),
                    reference["line"].as_u64().unwrap_or(0),
                    reference["text"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok_structured(
            truncate(rendered),
            json!({
                "symbol": symbol,
                "references": references,
                "count": references.len(),
                "confidence": "lexical",
                "truncated": references.len() >= limit || scan_truncated,
                "scanned_files": scanned_files,
            }),
        ))
    }
}

#[async_trait]
impl AgentTool for CodeDiagnostics {
    fn name(&self) -> &'static str {
        "code_diagnostics"
    }

    fn description(&self) -> &'static str {
        "Run the project's check or lint task and return structured diagnostics grouped by file. No LSP server is required."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Optional path filters"},
                "severity": {"type": "string", "enum": ["error", "warning", "all"], "description": "Default error"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Default 100"},
                "source": {"type": "string", "enum": ["auto", "build", "lsp"], "description": "Default auto; lsp is unavailable"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
        let source = input
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("auto");
        if source == "lsp" {
            return Ok(ToolOutput::error_structured(
                "no LSP backend is configured; use source=build or auto",
                json!({"code": "LSP_UNAVAILABLE"}),
            ));
        }
        let severity = input
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("error");
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 200) as usize;
        let paths = input.get("paths").and_then(Value::as_array);
        let mut run = run_detected_task(context, "check", 300).await?;
        if run.is_none() {
            run = run_detected_task(context, "lint", 300).await?;
        }
        let Some(run) = run else {
            let profile = detect_profile(&context.root);
            return Ok(ToolOutput::error_structured(
                "no check or lint command was detected",
                json!({
                    "code": "DIAGNOSTICS_UNAVAILABLE",
                    "detected_commands": profile.commands.keys().collect::<Vec<_>>(),
                }),
            ));
        };
        let mut diagnostics = run
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
                            .any(|filter| path.starts_with(filter))
                    })
                })
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        diagnostics.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.line.cmp(&right.line))
        });
        let mut grouped: BTreeMap<String, Vec<&crate::tooling::project::Failure>> = BTreeMap::new();
        for diagnostic in &diagnostics {
            grouped
                .entry(
                    diagnostic
                        .path
                        .clone()
                        .unwrap_or_else(|| "(project)".to_string()),
                )
                .or_default()
                .push(diagnostic);
        }
        let group_count = grouped.len();
        let rendered = grouped
            .iter()
            .map(|(path, diagnostics)| {
                let lines = diagnostics
                    .iter()
                    .map(|diagnostic| {
                        format!(
                            "  {}:{}{} {}",
                            path,
                            diagnostic.line.unwrap_or(0),
                            diagnostic
                                .column
                                .map(|column| format!(":{column}"))
                                .unwrap_or_default(),
                            diagnostic.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{path}\n{lines}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let ok = diagnostics.is_empty() && run.exit_code == 0;
        Ok(ToolOutput::ok_structured(
            truncate(if ok {
                format!("no diagnostics ({})", run.command)
            } else if rendered.is_empty() {
                format!(
                    "{} exited {} with no parsed diagnostics",
                    run.command, run.exit_code
                )
            } else {
                format!(
                    "{} ({}) {}\n{rendered}",
                    run.command, run.source, run.exit_code
                )
            }),
            json!({
                "ok": ok,
                "source": "build",
                "command": run.command,
                "command_source": run.source,
                "exit_code": run.exit_code,
                "diagnostics": diagnostics,
                "groups": group_count,
                "warning_count": run.warning_count,
                "duration_ms": run.duration_ms,
                "raw_tail": tail_chars(&run.output, 4_000),
                "truncated": diagnostics.len() >= limit,
                "confidence": "compiler-or-parser",
            }),
        ))
    }
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
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Default 12"}
            }
        })
    }

    async fn execute(&self, context: &ToolContext, input: Value) -> Result<ToolOutput> {
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
        let mut tests = affected_tests(&context.root, &changed);
        tests.truncate(limit);
        let profile = detect_profile(&context.root);
        let command = profile
            .commands
            .get("test")
            .map(|command| command.command.clone());
        let rendered = tests
            .iter()
            .map(|test| {
                format!(
                    "{} ({}, {:.0}%)",
                    test.path,
                    test.reason,
                    test.confidence * 100.0
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok_structured(
            truncate(if rendered.is_empty() {
                "no affected tests matched; run the full suite".to_string()
            } else {
                rendered
            }),
            json!({
                "changed_paths": changed,
                "tests": tests,
                "test_command": command,
                "warnings": if tests.is_empty() {
                    vec!["no mapping found; run the full test suite"]
                } else {
                    Vec::<&str>::new()
                },
                "confidence": "lexical",
            }),
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
        "Assemble a bounded, cited context packet for a goal from symbols, files, tests, project commands, and recent changes. No model call."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {"type": "string", "description": "What the agent is trying to do"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Optional focus paths"},
                "max_tokens": {"type": "integer", "minimum": 500, "maximum": 16000, "description": "Default 4000"},
                "include": {"type": "array", "items": {"type": "string", "enum": ["code", "tests", "git", "profile", "docs"]}, "description": "Sections to include"}
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
            selected_symbols.push(symbol.clone());
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
                        "reason": "symbol",
                    }));
                    format!(
                        "- {}:{} {} {}",
                        symbol.path, symbol.line, symbol.kind, symbol.name
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
            "{} chars · ~{} tokens · {} symbols · {} files",
            packet.len(),
            estimated_tokens,
            selected_symbols.len(),
            files_seen(&citations)
        );
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n\n{packet}")),
            json!({
                "goal": goal,
                "context": packet,
                "citations": citations,
                "estimated_tokens": estimated_tokens,
                "max_tokens": max_tokens,
                "truncated": truncated,
                "confidence": "lexical",
                "next_actions": ["read the cited files before editing"],
            }),
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
}

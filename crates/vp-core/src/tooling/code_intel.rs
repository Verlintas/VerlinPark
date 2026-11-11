//! Shared result contract for LSP-backed code intelligence.
//!
//! Code intelligence is optional and multi-backend. Every tool result must say
//! which backend produced it, whether the answer is exact or heuristic, and why
//! a fallback happened. This module owns those types and the deterministic
//! normalization/grouping rules so the lexical and LSP paths cannot drift.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};

/// Definition/reference results are exact at the language-server level, but no
/// language server is omniscient (generated code, macros, unindexed files), so
/// the score stays below 1.0 on purpose.
pub(crate) const LSP_CONFIDENCE_DEFINITION: f64 = 0.99;
pub(crate) const LSP_CONFIDENCE_REFERENCE: f64 = 0.98;
pub(crate) const LSP_CONFIDENCE_SYMBOL: f64 = 0.97;
pub(crate) const LSP_CONFIDENCE_DIAGNOSTIC: f64 = 0.96;
pub(crate) const LEXICAL_CONFIDENCE_SYMBOL: f64 = 0.75;
pub(crate) const LEXICAL_CONFIDENCE_REFERENCE: f64 = 0.55;
pub(crate) const BUILD_CONFIDENCE: f64 = 0.99;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Source {
    Lsp,
    Lexical,
    Build,
}

impl Source {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
            Self::Lexical => "lexical",
            Self::Build => "build",
        }
    }

    pub(crate) fn confidence_label(self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
            Self::Lexical => "lexical",
            Self::Build => "compiler-or-parser",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Ok,
    Partial,
    Fallback,
    Unavailable,
    Unsupported,
    Timeout,
    Crashed,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Fallback => "fallback",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::Timeout => "timeout",
            Self::Crashed => "crashed",
        }
    }

    pub(crate) fn is_success(self) -> bool {
        matches!(self, Self::Ok | Self::Partial)
    }
}

/// User-requested backend policy for tools that have both LSP and lexical or
/// build paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceChoice {
    Auto,
    Lsp,
    Lexical,
    Build,
}

impl SourceChoice {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "lsp" => Some(Self::Lsp),
            "lexical" => Some(Self::Lexical),
            "build" => Some(Self::Build),
            _ => None,
        }
    }
}

/// One attempted backend during a multi-language query.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct BackendAttempt {
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
}

/// Machine-readable backend envelope shared by symbol, reference, diagnostic,
/// map, and context results.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Backend {
    pub source: Source,
    pub status: Status,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<BackendAttempt>,
}

impl Backend {
    pub(crate) fn lsp(language: &str, server: Option<&str>, method: &str, status: Status) -> Self {
        Self {
            source: Source::Lsp,
            status,
            score: LSP_CONFIDENCE_SYMBOL,
            language: Some(language.to_string()),
            server: server.map(str::to_string),
            server_version: None,
            method: Some(method.to_string()),
            fallback_reason: None,
            install_hint: None,
            attempts: Vec::new(),
        }
    }

    pub(crate) fn lexical(status: Status, score: f64) -> Self {
        Self {
            source: Source::Lexical,
            status,
            score,
            language: None,
            server: None,
            server_version: None,
            method: Some("lexical_index".to_string()),
            fallback_reason: None,
            install_hint: None,
            attempts: Vec::new(),
        }
    }

    pub(crate) fn build(status: Status) -> Self {
        Self {
            source: Source::Build,
            status,
            score: BUILD_CONFIDENCE,
            language: None,
            server: None,
            server_version: None,
            method: Some("detected_check".to_string()),
            fallback_reason: None,
            install_hint: None,
            attempts: Vec::new(),
        }
    }

    pub(crate) fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    pub(crate) fn with_server_version(mut self, version: impl Into<String>) -> Self {
        self.server_version = Some(version.into());
        self
    }

    pub(crate) fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    pub(crate) fn with_score(mut self, score: f64) -> Self {
        self.score = score.clamp(0.0, 1.0);
        self
    }

    pub(crate) fn with_fallback(mut self, reason: impl Into<String>, hint: Option<String>) -> Self {
        self.fallback_reason = Some(reason.into());
        self.install_hint = hint;
        self
    }

    pub(crate) fn with_attempts(mut self, attempts: Vec<BackendAttempt>) -> Self {
        self.attempts = attempts;
        self
    }

    pub(crate) fn as_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"source": self.source.as_str()}))
    }

    /// Add the shared contract fields to an existing structured result.
    ///
    /// `confidence` stays a categorical string for compatibility with the first
    /// lexical tools; `confidence_score` is the numeric companion. `source` and
    /// `status` are additive and `serde(default)`-safe on the client side.
    pub(crate) fn apply(&self, value: &mut Value) {
        let Some(object) = value.as_object_mut() else {
            return;
        };
        object.insert("source".to_string(), json!(self.source.as_str()));
        object.insert("status".to_string(), json!(self.status.as_str()));
        object.insert(
            "confidence".to_string(),
            json!(self.source.confidence_label()),
        );
        object.insert(
            "confidence_score".to_string(),
            json!(round_confidence(self.score)),
        );
        if let Some(language) = &self.language {
            object.insert("language".to_string(), json!(language));
        }
        if let Some(server) = &self.server {
            object.insert("language_server".to_string(), json!(server));
        }
        if let Some(version) = &self.server_version {
            object.insert("language_server_version".to_string(), json!(version));
        }
        if let Some(method) = &self.method {
            object.insert("method".to_string(), json!(method));
        }
        if let Some(reason) = &self.fallback_reason {
            object.insert("fallback_reason".to_string(), json!(reason));
        }
        if let Some(hint) = &self.install_hint {
            object.insert("install_hint".to_string(), json!(hint));
        }
        object.insert("backend".to_string(), self.as_json());
    }
}

pub(crate) fn round_confidence(value: f64) -> f64 {
    (value.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CodeRange {
    pub line: u64,
    pub column: u64,
    pub end_line: u64,
    pub end_column: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodeSymbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub kind_group: String,
    pub language: String,
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub end_line: u64,
    pub end_column: u64,
    pub range: CodeRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub signature: String,
    pub score: f64,
    pub source: String,
    pub confidence: String,
    pub confidence_score: f64,
}

impl CodeSymbol {
    fn dedupe_key(&self) -> (String, u64, u64, u64, u64, String, String) {
        (
            self.path.clone(),
            self.line,
            self.column,
            self.end_line,
            self.end_column,
            self.name.clone(),
            self.kind.clone(),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodeLocation {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub end_line: u64,
    pub end_column: u64,
    pub range: CodeRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub is_definition: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub source: String,
    pub confidence: String,
    pub confidence_score: f64,
}

impl CodeLocation {
    fn dedupe_key(&self) -> (String, u64, u64, u64, u64) {
        (
            self.path.clone(),
            self.line,
            self.column,
            self.end_line,
            self.end_column,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodeDiagnostic {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub end_line: u64,
    pub end_column: u64,
    pub range: CodeRange,
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub message: String,
    pub backend: String,
    pub confidence_score: f64,
}

impl CodeDiagnostic {
    fn dedupe_key(&self) -> (String, u64, u64, u64, u64, String, String, String) {
        (
            self.path.clone(),
            self.line,
            self.column,
            self.end_line,
            self.end_column,
            self.severity.clone(),
            self.message.clone(),
            self.code
                .as_ref()
                .map(|code| code.to_string())
                .unwrap_or_default(),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DiagnosticGroup {
    pub path: String,
    pub error_count: u64,
    pub warning_count: u64,
    pub information_count: u64,
    pub hint_count: u64,
    pub diagnostics: Vec<CodeDiagnostic>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DiagnosticSummary {
    pub groups: Vec<DiagnosticGroup>,
    pub total: usize,
    pub errors: u64,
    pub warnings: u64,
    pub information: u64,
    pub hints: u64,
    pub truncated: bool,
}

impl DiagnosticSummary {
    pub(crate) fn ok(&self) -> bool {
        self.errors == 0
    }
}

pub(crate) fn severity_rank(severity: &str) -> u8 {
    match severity {
        "error" => 0,
        "warning" => 1,
        "information" | "info" => 2,
        "hint" => 3,
        _ => 4,
    }
}

pub(crate) fn kind_group(kind: &str) -> &'static str {
    match kind {
        "function" | "method" | "constructor" | "operator" => "function",
        "class" | "struct" | "enum" | "interface" | "trait" | "type" | "typealias" | "impl"
        | "actor" | "record" => "type",
        "module" | "namespace" | "package" | "file" => "module",
        "constant" | "variable" | "field" | "property" | "enum_member" => "constant",
        "macro" => "macro",
        _ => "symbol",
    }
}

pub(crate) fn display_language(id: &str) -> &'static str {
    match id {
        "rust" => "Rust",
        "typescript" => "TypeScript",
        "javascript" => "JavaScript",
        "python" => "Python",
        "go" => "Go",
        "swift" => "Swift",
        "c" => "C",
        "cpp" => "C++",
        "java" => "Java",
        "kotlin" => "Kotlin",
        _ => "Unknown",
    }
}

/// Deterministic name relevance used by both the lexical index and LSP symbol
/// filtering. The exact scoring curve is intentionally simple and stable.
pub(crate) fn score_name_match(name: &str, query: &str) -> f64 {
    if query.trim().is_empty() {
        return 1.0;
    }
    let name_lower = name.to_ascii_lowercase();
    let query_lower = query.trim().to_ascii_lowercase();
    if name == query {
        1.0
    } else if name_lower == query_lower {
        0.95
    } else if name_lower.starts_with(&query_lower) {
        0.9
    } else if name_lower.contains(&query_lower) {
        0.75
    } else if query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .all(|token| name_lower.contains(&token.to_ascii_lowercase()))
    {
        0.65
    } else {
        0.0
    }
}

fn compare_symbols(left: &CodeSymbol, right: &CodeSymbol) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| left.line.cmp(&right.line))
        .then_with(|| left.column.cmp(&right.column))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.kind.cmp(&right.kind))
}

fn compare_symbols_by_location(left: &CodeSymbol, right: &CodeSymbol) -> std::cmp::Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.line.cmp(&right.line))
        .then_with(|| left.column.cmp(&right.column))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.kind.cmp(&right.kind))
}

/// Remove cross-server duplicates, sort deterministically, and cap. When a
/// query is supplied the highest-scoring duplicate is retained.
pub(crate) fn prepare_symbols(
    mut symbols: Vec<CodeSymbol>,
    query: Option<&str>,
    limit: usize,
) -> (Vec<CodeSymbol>, bool) {
    if let Some(query) = query {
        for symbol in &mut symbols {
            if symbol.score <= 0.0 {
                symbol.score = score_name_match(&symbol.name, query);
            }
        }
        symbols.sort_by(compare_symbols);
    } else {
        symbols.sort_by(compare_symbols_by_location);
    }
    let mut seen = HashSet::new();
    symbols.retain(|symbol| seen.insert(symbol.dedupe_key()));
    let truncated = symbols.len() > limit;
    symbols.truncate(limit);
    (symbols, truncated)
}

pub(crate) fn prepare_locations(
    mut locations: Vec<CodeLocation>,
    limit: usize,
) -> (Vec<CodeLocation>, bool) {
    locations.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| left.end_line.cmp(&right.end_line))
            .then_with(|| left.end_column.cmp(&right.end_column))
    });
    let mut seen = HashSet::new();
    locations.retain(|location| seen.insert(location.dedupe_key()));
    let truncated = locations.len() > limit;
    locations.truncate(limit);
    (locations, truncated)
}

/// Group diagnostics by file and severity. Input is sorted before grouping so
/// two servers reporting the same file produce a stable transcript.
pub(crate) fn prepare_diagnostics(
    mut diagnostics: Vec<CodeDiagnostic>,
    limit: usize,
) -> DiagnosticSummary {
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| severity_rank(&left.severity).cmp(&severity_rank(&right.severity)))
            .then_with(|| left.message.cmp(&right.message))
            .then_with(|| left.source.cmp(&right.source))
    });
    let mut seen = HashSet::new();
    diagnostics.retain(|diagnostic| seen.insert(diagnostic.dedupe_key()));
    let truncated = diagnostics.len() > limit;
    diagnostics.truncate(limit);
    let mut summary = DiagnosticSummary {
        truncated,
        ..DiagnosticSummary::default()
    };
    let mut grouped: BTreeMap<String, Vec<CodeDiagnostic>> = BTreeMap::new();
    for diagnostic in diagnostics {
        summary.total += 1;
        match diagnostic.severity.as_str() {
            "error" => summary.errors += 1,
            "warning" => summary.warnings += 1,
            "information" | "info" => summary.information += 1,
            "hint" => summary.hints += 1,
            _ => {}
        }
        grouped
            .entry(diagnostic.path.clone())
            .or_default()
            .push(diagnostic);
    }
    summary.groups = grouped
        .into_iter()
        .map(|(path, diagnostics)| {
            let mut group = DiagnosticGroup {
                path,
                error_count: 0,
                warning_count: 0,
                information_count: 0,
                hint_count: 0,
                diagnostics,
            };
            for diagnostic in &group.diagnostics {
                match diagnostic.severity.as_str() {
                    "error" => group.error_count += 1,
                    "warning" => group.warning_count += 1,
                    "information" | "info" => group.information_count += 1,
                    "hint" => group.hint_count += 1,
                    _ => {}
                }
            }
            group
        })
        .collect();
    summary
}

pub(crate) fn render_symbols(symbols: &[CodeSymbol]) -> String {
    symbols
        .iter()
        .map(|symbol| {
            let mut line = format!(
                "{}:{}:{} · {} {}",
                symbol.path, symbol.line, symbol.column, symbol.kind, symbol.name
            );
            if let Some(container) = &symbol.container {
                line.push_str(&format!(" · in {container}"));
            }
            if !symbol.signature.is_empty() {
                line.push_str(&format!(" · {}", symbol.signature));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn render_locations(locations: &[CodeLocation]) -> String {
    locations
        .iter()
        .map(|location| {
            let marker = if location.is_definition {
                " [declaration]"
            } else {
                ""
            };
            format!(
                "{}:{}:{}{} {}",
                location.path,
                location.line,
                location.column,
                marker,
                location.context.as_deref().unwrap_or("")
            )
            .trim_end()
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn render_diagnostics(summary: &DiagnosticSummary) -> String {
    summary
        .groups
        .iter()
        .map(|group| {
            let lines = group
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    let code = diagnostic
                        .code
                        .as_ref()
                        .map(|code| match code {
                            Value::String(value) => format!(" [{value}]"),
                            other => format!(" [{other}]"),
                        })
                        .unwrap_or_default();
                    let source = match (diagnostic.source.as_deref(), diagnostic.server.as_deref())
                    {
                        (Some(source), Some(server)) if source != server => {
                            format!(" ({source} · {server})")
                        }
                        (Some(source), _) => format!(" ({source})"),
                        (None, Some(server)) => format!(" ({server})"),
                        (None, None) => String::new(),
                    };
                    format!(
                        "  {}:{}:{} {}{}{} {}",
                        diagnostic.path,
                        diagnostic.line,
                        diagnostic.column,
                        diagnostic.severity,
                        code,
                        source,
                        diagnostic.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n{lines}", group.path)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Human-readable backend sentence used at the top of provider-facing text.
pub(crate) fn backend_sentence(backend: &Backend) -> String {
    match backend.source {
        Source::Lsp => {
            let server = backend.server.as_deref().unwrap_or("language server");
            let method = backend.method.as_deref().unwrap_or("query");
            format!("LSP · {server} · {method} · {}", backend.status.as_str())
        }
        Source::Lexical => match &backend.fallback_reason {
            Some(reason) => format!(
                "lexical fallback ({reason}) · not compiler-accurate · {}",
                backend.status.as_str()
            ),
            None => "lexical index · best-effort, not compiler-accurate".to_string(),
        },
        Source::Build => format!(
            "build diagnostics · {} · {}",
            backend.method.as_deref().unwrap_or("detected check"),
            backend.status.as_str()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn symbol(name: &str, path: &str, line: u64, column: u64, score: f64) -> CodeSymbol {
        CodeSymbol {
            id: format!("{path}:{line}:{name}"),
            name: name.to_string(),
            kind: "function".to_string(),
            kind_group: "function".to_string(),
            language: "Rust".to_string(),
            path: path.to_string(),
            line,
            column,
            end_line: line + 2,
            end_column: 1,
            range: CodeRange {
                line,
                column,
                end_line: line + 2,
                end_column: 1,
            },
            container: None,
            visibility: None,
            signature: format!("fn {name}()"),
            score,
            source: "lsp".to_string(),
            confidence: "lsp".to_string(),
            confidence_score: LSP_CONFIDENCE_SYMBOL,
        }
    }

    fn location(path: &str, line: u64, column: u64) -> CodeLocation {
        CodeLocation {
            path: path.to_string(),
            line,
            column,
            end_line: line,
            end_column: column + 3,
            range: CodeRange {
                line,
                column,
                end_line: line,
                end_column: column + 3,
            },
            context: Some("let x = 1;".to_string()),
            is_definition: false,
            kind: Some("function".to_string()),
            source: "lsp".to_string(),
            confidence: "lsp".to_string(),
            confidence_score: LSP_CONFIDENCE_REFERENCE,
        }
    }

    fn diagnostic(path: &str, line: u64, severity: &str, message: &str) -> CodeDiagnostic {
        CodeDiagnostic {
            path: path.to_string(),
            line,
            column: 1,
            end_line: line,
            end_column: 5,
            range: CodeRange {
                line,
                column: 1,
                end_line: line,
                end_column: 5,
            },
            severity: severity.to_string(),
            code: Some(json!("E0001")),
            source: Some("rustc".to_string()),
            server: Some("rust-analyzer".to_string()),
            message: message.to_string(),
            backend: "lsp".to_string(),
            confidence_score: LSP_CONFIDENCE_DIAGNOSTIC,
        }
    }

    #[test]
    fn source_and_status_labels_are_stable() {
        assert_eq!(Source::Lsp.as_str(), "lsp");
        assert_eq!(Source::Lexical.as_str(), "lexical");
        assert_eq!(Source::Build.as_str(), "build");
        assert_eq!(Status::Fallback.as_str(), "fallback");
        assert_eq!(Status::Unsupported.as_str(), "unsupported");
        assert!(Status::Ok.is_success());
        assert!(!Status::Crashed.is_success());
        assert_eq!(SourceChoice::parse("LSP"), Some(SourceChoice::Lsp));
        assert_eq!(SourceChoice::parse(""), Some(SourceChoice::Auto));
        assert_eq!(SourceChoice::parse("model"), None);
    }

    #[test]
    fn backend_apply_keeps_legacy_confidence_and_adds_contract() {
        let backend = Backend::lsp(
            "rust",
            Some("rust-analyzer"),
            "workspace/symbol",
            Status::Ok,
        )
        .with_server_version("1.0")
        .with_attempts(vec![BackendAttempt {
            language: "rust".into(),
            server: Some("rust-analyzer".into()),
            method: Some("workspace/symbol".into()),
            status: "ok".into(),
            message: None,
            install_hint: None,
        }]);
        let mut value = json!({"symbols": []});
        backend.apply(&mut value);
        assert_eq!(value["source"], "lsp");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["confidence"], "lsp");
        assert_eq!(value["confidence_score"], 0.97);
        assert_eq!(value["backend"]["server"], "rust-analyzer");
        assert_eq!(value["backend"]["attempts"][0]["status"], "ok");
        assert_eq!(value["language"], "rust");
    }

    #[test]
    fn lexical_fallback_is_labeled_not_exact() {
        let backend = Backend::lexical(Status::Fallback, LEXICAL_CONFIDENCE_SYMBOL)
            .with_fallback("lsp_unavailable", Some("install rust-analyzer".into()));
        let mut value = json!({});
        backend.apply(&mut value);
        assert_eq!(value["source"], "lexical");
        assert_eq!(value["status"], "fallback");
        assert_eq!(value["fallback_reason"], "lsp_unavailable");
        assert_eq!(value["install_hint"], "install rust-analyzer");
        assert!(value["confidence_score"].as_f64().unwrap() < 0.8);
        assert!(backend_sentence(&backend).contains("lexical fallback"));
        assert!(backend_sentence(&backend).contains("not compiler-accurate"));
    }

    #[test]
    fn symbol_preparation_dedupes_keeps_best_score_and_sorts() {
        let mut low = symbol("Widget", "src/b.rs", 4, 1, 0.4);
        low.score = score_name_match("Widget", "widget");
        let high = symbol("Widget", "src/b.rs", 4, 1, 0.95);
        let other = symbol("widget_builder", "src/a.rs", 9, 3, 0.7);
        let (symbols, truncated) =
            prepare_symbols(vec![low, high.clone(), other], Some("widget"), 10);
        assert!(!truncated);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].path, "src/b.rs");
        assert_eq!(symbols[0].score, 0.95);
        assert_eq!(symbols[1].path, "src/a.rs");

        let other = symbol("other", "src/c.rs", 1, 1, 0.1);
        let (capped, truncated) = prepare_symbols(vec![high, other], None, 1);
        assert!(truncated);
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn location_preparation_is_deterministic_and_bounded() {
        let mut locations = vec![
            location("src/z.rs", 2, 1),
            location("src/a.rs", 10, 4),
            location("src/a.rs", 10, 4),
            location("src/a.rs", 2, 8),
        ];
        locations[2].context = Some("duplicate".into());
        let (locations, truncated) = prepare_locations(locations, 2);
        assert!(truncated);
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].path, "src/a.rs");
        assert_eq!(locations[0].line, 2);
        assert_eq!(locations[1].line, 10);
        assert_eq!(locations[1].context.as_deref(), Some("let x = 1;"));
    }

    #[test]
    fn diagnostics_group_by_file_and_severity() {
        let summary = prepare_diagnostics(
            vec![
                diagnostic("src/a.rs", 3, "warning", "unused"),
                diagnostic("src/b.rs", 1, "error", "missing"),
                diagnostic("src/a.rs", 1, "error", "broken"),
                diagnostic("src/a.rs", 1, "error", "broken"),
            ],
            100,
        );
        assert_eq!(summary.total, 3);
        assert_eq!(summary.errors, 2);
        assert_eq!(summary.warnings, 1);
        assert!(!summary.ok());
        assert_eq!(summary.groups.len(), 2);
        assert_eq!(summary.groups[0].path, "src/a.rs");
        assert_eq!(summary.groups[0].error_count, 1);
        assert_eq!(summary.groups[0].warning_count, 1);
        assert_eq!(summary.groups[1].path, "src/b.rs");
        let rendered = render_diagnostics(&summary);
        assert!(rendered.contains("src/a.rs"));
        assert!(rendered.contains("E0001"));
        assert!(rendered.contains("rust-analyzer"));
    }

    #[test]
    fn diagnostic_cap_is_explicit() {
        let diagnostics = (0..10)
            .map(|index| diagnostic("src/a.rs", index + 1, "error", &format!("error {index}")))
            .collect::<Vec<_>>();
        let summary = prepare_diagnostics(diagnostics, 4);
        assert!(summary.truncated);
        assert_eq!(summary.total, 4);
        assert_eq!(summary.errors, 4);
    }

    #[test]
    fn kind_groups_cover_lsp_names() {
        assert_eq!(kind_group("method"), "function");
        assert_eq!(kind_group("struct"), "type");
        assert_eq!(kind_group("module"), "module");
        assert_eq!(kind_group("enum_member"), "constant");
        assert_eq!(kind_group("macro"), "macro");
        assert_eq!(kind_group("unknown"), "symbol");
    }

    #[test]
    fn name_match_scores_are_monotonic() {
        assert_eq!(score_name_match("Widget", "Widget"), 1.0);
        assert_eq!(score_name_match("Widget", "widget"), 0.95);
        assert_eq!(score_name_match("WidgetBuilder", "widget"), 0.9);
        assert_eq!(score_name_match("build_widget", "widget"), 0.75);
        assert_eq!(score_name_match("build_widget", "build widget"), 0.65);
        assert_eq!(score_name_match("other", "widget"), 0.0);
        assert_eq!(score_name_match("other", ""), 1.0);
    }

    #[test]
    fn renders_symbols_with_ranges_and_containers() {
        let mut symbol = symbol("Widget", "src/lib.rs", 4, 12, 0.97);
        symbol.container = Some("impl Widget".into());
        let rendered = render_symbols(&[symbol]);
        assert!(rendered.contains("src/lib.rs:4:12"));
        assert!(rendered.contains("function Widget"));
        assert!(rendered.contains("in impl Widget"));
        assert!(rendered.contains("fn Widget()"));
    }

    #[test]
    fn build_backend_is_compiler_labeled() {
        let backend = Backend::build(Status::Ok);
        let mut value = json!({});
        backend.apply(&mut value);
        assert_eq!(value["source"], "build");
        assert_eq!(value["confidence"], "compiler-or-parser");
        assert_eq!(value["confidence_score"], 0.99);
        assert!(backend_sentence(&backend).contains("build diagnostics"));
    }

    #[test]
    fn language_display_covers_supported_servers() {
        assert_eq!(display_language("rust"), "Rust");
        assert_eq!(display_language("cpp"), "C++");
        assert_eq!(display_language("javascript"), "JavaScript");
        assert_eq!(display_language("unknown"), "Unknown");
    }
}

//! Bounded structured-data inspection.
//!
//! Agents frequently need one field from a JSON/YAML/TOML config or a few CSV
//! rows. Dumping the whole file wastes context and invites shell parsing. This
//! tool parses the data, applies a small deterministic path query, and returns
//! only matching values plus a structural summary.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::{AgentTool, ToolContext, ToolOutput, safe_existing_path, truncate};

const MAX_DATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RESULTS: usize = 500;
const MAX_NODES: usize = 20_000;
const MAX_VALUE_CHARS: usize = 4_000;
const MAX_CSV_ROWS: usize = 100_000;

pub struct DataQuery;

#[derive(Debug, Clone)]
enum Segment {
    Key(String),
    Index(usize),
    Wildcard,
    Recursive(String),
}

#[async_trait]
impl AgentTool for DataQuery {
    fn name(&self) -> &'static str {
        "data_query"
    }

    fn description(&self) -> &'static str {
        "Query JSON, YAML, TOML, or CSV with a deterministic path expression. Returns matching values and a bounded structural summary."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Data file relative to the worktree root"},
                "format": {"type": "string", "enum": ["auto", "json", "yaml", "toml", "csv"], "description": "Default auto by extension/content"},
                "query": {"type": "string", "description": "Path expression such as dependencies.serde.version, items[*].name, or ..version"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "description": "Default 50"},
                "offset": {"type": "integer", "minimum": 0, "description": "Result offset; default 0"},
                "columns": {"type": "array", "items": {"type": "string"}, "description": "CSV columns to include; default all"},
                "pretty": {"type": "boolean", "description": "Pretty-print object results; default false"}
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
        if !path.is_file() {
            return Ok(ToolOutput::error_structured(
                "data_query path must be a file",
                json!({"code": "INVALID_ARGUMENT", "path": relative}),
            ));
        }
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_DATA_BYTES {
            return Ok(ToolOutput::error_structured(
                format!(
                    "data file exceeds the {} MiB limit",
                    MAX_DATA_BYTES / 1024 / 1024
                ),
                json!({"code": "FILE_TOO_LARGE", "size_bytes": metadata.len()}),
            ));
        }
        let bytes = tokio::fs::read(&path).await?;
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            return Ok(ToolOutput::error_structured(
                "data_query does not read binary files",
                json!({"code": "UNSUPPORTED_CONTENT", "path": relative}),
            ));
        }
        let text = String::from_utf8(bytes).map_err(|_| anyhow!("data file is not UTF-8"))?;
        let format = input
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("auto");
        let format = if format == "auto" {
            detect_format(&path, &text).to_string()
        } else {
            format.to_string()
        };
        let columns = input
            .get("columns")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (value, kind) = match format.as_str() {
            "json" => (
                serde_json::from_str::<Value>(&text).context("invalid JSON")?,
                "json",
            ),
            "yaml" => {
                let yaml: serde_yaml::Value =
                    serde_yaml::from_str(&text).context("invalid YAML")?;
                (
                    serde_json::to_value(yaml).context("converting YAML")?,
                    "yaml",
                )
            }
            "toml" => {
                let toml: toml::Value = toml::from_str(&text).context("invalid TOML")?;
                (
                    serde_json::to_value(toml).context("converting TOML")?,
                    "toml",
                )
            }
            "csv" => {
                let (headers, mut rows) = parse_csv(&text)?;
                if !columns.is_empty() {
                    let selected = columns
                        .iter()
                        .filter(|column| headers.contains(*column))
                        .cloned()
                        .collect::<Vec<_>>();
                    if selected.is_empty() {
                        return Ok(ToolOutput::error_structured(
                            "columns do not match CSV headers",
                            json!({"code": "INVALID_ARGUMENT", "columns": columns, "headers": headers}),
                        ));
                    }
                    rows = rows
                        .into_iter()
                        .map(|row| {
                            selected
                                .iter()
                                .map(|column| {
                                    (
                                        column.clone(),
                                        row.get(column).cloned().unwrap_or(Value::Null),
                                    )
                                })
                                .collect::<serde_json::Map<_, _>>()
                        })
                        .map(Value::Object)
                        .collect();
                }
                (
                    json!({
                        "columns": headers,
                        "rows": rows,
                        "row_count": rows.len(),
                    }),
                    "csv",
                )
            }
            other => {
                return Ok(ToolOutput::error_structured(
                    format!("unsupported data format: {other}"),
                    json!({"code": "INVALID_ARGUMENT", "format": other}),
                ));
            }
        };
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, MAX_RESULTS as u64) as usize;
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let pretty = input
            .get("pretty")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty());
        if let Some(query) = query {
            let segments = match parse_query(query) {
                Ok(segments) => segments,
                Err(error) => {
                    return Ok(ToolOutput::error_structured(
                        format!("invalid query: {error}"),
                        json!({"code": "INVALID_ARGUMENT", "query": query}),
                    ));
                }
            };
            let all = evaluate(&value, &segments);
            let total = all.len();
            let results = all
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|(path, value)| {
                    json!({
                        "path": path,
                        "value": value,
                        "value_type": value_type(&value),
                    })
                })
                .collect::<Vec<_>>();
            let rendered = results
                .iter()
                .map(|result| {
                    format!(
                        "{} = {}",
                        result["path"].as_str().unwrap_or("?"),
                        render_value(&result["value"], pretty)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(ToolOutput::ok_structured(
                truncate(if rendered.is_empty() {
                    format!("no values matched {query}")
                } else {
                    rendered
                }),
                json!({
                    "path": relative,
                    "format": format,
                    "query": query,
                    "results": results,
                    "count": results.len(),
                    "total_matches": total,
                    "offset": offset,
                    "truncated": offset.saturating_add(results.len()) < total,
                    "confidence": "deterministic",
                }),
            ));
        }
        let preview = preview_value(&value, limit);
        let summary = match &value {
            Value::Object(object) => format!(
                "{} · object with {} key(s){}{}",
                relative,
                object.len(),
                if columns.is_empty() {
                    String::new()
                } else {
                    format!(" · columns {}", columns.join(","))
                },
                if format == "csv" {
                    format!(
                        " · {} row(s)",
                        value.get("row_count").and_then(Value::as_u64).unwrap_or(0)
                    )
                } else {
                    String::new()
                }
            ),
            Value::Array(array) => format!("{} · array with {} item(s)", relative, array.len()),
            _ => format!("{} · {}", relative, value_type(&value)),
        };
        let structured = json!({
            "path": relative,
            "format": format,
            "kind": kind,
            "summary": summary,
            "preview": preview,
            "size_bytes": metadata.len(),
        });
        Ok(ToolOutput::ok_structured(
            truncate(format!("{summary}\n{}", render_value(&preview, true))),
            structured,
        ))
    }
}

fn detect_format(path: &Path, text: &str) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" | "jsonl" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "csv" => "csv",
        _ => {
            let trimmed = text.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                "json"
            } else if trimmed.contains('=') && !trimmed.contains(',') {
                "toml"
            } else if text.lines().next().is_some_and(|line| line.contains(',')) {
                "csv"
            } else {
                "yaml"
            }
        }
    }
}

fn parse_query(query: &str) -> Result<Vec<Segment>> {
    let mut chars = query.chars().peekable();
    let mut segments = Vec::new();
    if chars.peek() == Some(&'$') {
        chars.next();
    }
    while let Some(ch) = chars.peek().copied() {
        match ch {
            '.' => {
                chars.next();
                if chars.peek() == Some(&'.') {
                    chars.next();
                    let key = read_key(&mut chars)?;
                    if key.is_empty() {
                        bail!("recursive descent requires a key");
                    }
                    segments.push(Segment::Recursive(key));
                } else {
                    let key = read_key(&mut chars)?;
                    if key.is_empty() {
                        bail!("empty path segment");
                    }
                    segments.push(Segment::Key(key));
                }
            }
            '[' => {
                chars.next();
                let mut inner = String::new();
                for ch in chars.by_ref() {
                    if ch == ']' {
                        break;
                    }
                    inner.push(ch);
                }
                let inner = inner.trim();
                if inner == "*" {
                    segments.push(Segment::Wildcard);
                } else if let Ok(index) = inner.parse::<usize>() {
                    segments.push(Segment::Index(index));
                } else if (inner.starts_with('"') && inner.ends_with('"'))
                    || (inner.starts_with('\'') && inner.ends_with('\''))
                {
                    segments.push(Segment::Key(inner[1..inner.len() - 1].to_string()));
                } else {
                    bail!("invalid bracket segment: [{inner}]");
                }
            }
            _ => {
                let key = read_key(&mut chars)?;
                if key.is_empty() {
                    bail!("unexpected character: {ch}");
                }
                segments.push(Segment::Key(key));
            }
        }
    }
    if segments.is_empty() {
        bail!("query is empty");
    }
    Ok(segments)
}

fn read_key<I>(chars: &mut std::iter::Peekable<I>) -> Result<String>
where
    I: Iterator<Item = char>,
{
    let mut key = String::new();
    while let Some(ch) = chars.peek().copied() {
        if ch == '.' || ch == '[' {
            break;
        }
        if !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | ':' | '/')) {
            bail!("unsupported key character: {ch}");
        }
        key.push(ch);
        chars.next();
    }
    Ok(key)
}

fn evaluate(root: &Value, segments: &[Segment]) -> Vec<(String, Value)> {
    let mut current = vec![("$".to_string(), root.clone())];
    let mut visited = 0usize;
    for segment in segments {
        let mut next = Vec::new();
        for (path, value) in &current {
            match segment {
                Segment::Key(key) => {
                    if let Some(child) = value.get(key) {
                        next.push((format!("{path}.{key}"), child.clone()));
                    }
                }
                Segment::Index(index) => {
                    if let Some(child) = value.get(*index) {
                        next.push((format!("{path}[{index}]"), child.clone()));
                    }
                }
                Segment::Wildcard => match value {
                    Value::Array(array) => {
                        for (index, child) in array.iter().enumerate() {
                            next.push((format!("{path}[{index}]"), child.clone()));
                            visited += 1;
                            if visited > MAX_NODES {
                                break;
                            }
                        }
                    }
                    Value::Object(object) => {
                        for (key, child) in object {
                            next.push((format!("{path}.{key}"), child.clone()));
                            visited += 1;
                            if visited > MAX_NODES {
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                Segment::Recursive(key) => {
                    collect_recursive(value, key, path, &mut next, &mut visited);
                }
            }
            if visited > MAX_NODES || next.len() > MAX_NODES {
                break;
            }
        }
        current = next;
        if current.is_empty() || visited > MAX_NODES {
            break;
        }
    }
    current.into_iter().take(MAX_RESULTS).collect()
}

fn collect_recursive(
    value: &Value,
    key: &str,
    path: &str,
    output: &mut Vec<(String, Value)>,
    visited: &mut usize,
) {
    if *visited > MAX_NODES {
        return;
    }
    match value {
        Value::Object(object) => {
            for (child_key, child) in object {
                let child_path = format!("{path}.{child_key}");
                *visited += 1;
                if child_key == key {
                    output.push((child_path.clone(), child.clone()));
                }
                collect_recursive(child, key, &child_path, output, visited);
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                let child_path = format!("{path}[{index}]");
                *visited += 1;
                collect_recursive(child, key, &child_path, output, visited);
            }
        }
        _ => {}
    }
}

fn preview_value(value: &Value, limit: usize) -> Value {
    match value {
        Value::Object(object) => {
            let mut map = serde_json::Map::new();
            for (key, value) in object.iter().take(limit.min(20)) {
                let preview = match value {
                    Value::Array(array) => json!(format!("[{} items]", array.len())),
                    Value::Object(object) => json!(format!("{{{} keys}}", object.len())),
                    Value::String(text) => json!(text.chars().take(200).collect::<String>()),
                    other => other.clone(),
                };
                map.insert(key.clone(), preview);
            }
            Value::Object(map)
        }
        Value::Array(array) => Value::Array(array.iter().take(limit.min(10)).cloned().collect()),
        other => other.clone(),
    }
}

fn render_value(value: &Value, pretty: bool) -> String {
    match value {
        Value::String(text) => text.chars().take(MAX_VALUE_CHARS).collect(),
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => {
            let mut text = if pretty {
                serde_json::to_string_pretty(other).unwrap_or_default()
            } else {
                serde_json::to_string(other).unwrap_or_default()
            };
            if text.chars().count() > MAX_VALUE_CHARS {
                text = text.chars().take(MAX_VALUE_CHARS).collect::<String>();
                text.push('…');
            }
            text
        }
    }
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn parse_csv(text: &str) -> Result<(Vec<String>, Vec<Value>)> {
    let records = parse_csv_records(text)?;
    if records.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut headers = records[0].clone();
    for (index, header) in headers.iter_mut().enumerate() {
        if header.trim().is_empty() {
            *header = format!("column_{}", index + 1);
        } else {
            *header = header.trim().to_string();
        }
    }
    let mut rows = Vec::new();
    for (row_index, record) in records.iter().skip(1).take(MAX_CSV_ROWS).enumerate() {
        if record.len() != headers.len() {
            return Err(anyhow!(
                "CSV row {} has {} fields, expected {}",
                row_index + 2,
                record.len(),
                headers.len()
            ));
        }
        let mut map = serde_json::Map::new();
        for (header, value) in headers.iter().zip(record) {
            map.insert(header.clone(), Value::String(value.clone()));
        }
        rows.push(Value::Object(map));
    }
    Ok((headers, rows))
}

fn parse_csv_records(text: &str) -> Result<Vec<Vec<String>>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' => {
                if !field.is_empty() {
                    bail!("unexpected quote in CSV field");
                }
                quoted = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(ch),
        }
    }
    if quoted {
        bail!("unterminated quoted CSV field");
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
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
        }
    }

    #[test]
    fn parses_paths_and_wildcards() {
        let value = json!({"dependencies": {"serde": {"version": "1.0"}}, "items": [{"name": "a"}, {"name": "b"}]});
        let segments = parse_query("dependencies.serde.version").unwrap();
        let results = evaluate(&value, &segments);
        assert_eq!(results[0].1, "1.0");
        let segments = parse_query("items[*].name").unwrap();
        let results = evaluate(&value, &segments);
        assert_eq!(results.len(), 2);
        let segments = parse_query("$..version").unwrap();
        let results = evaluate(&value, &segments);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn parses_csv_quotes_and_crlf() {
        let (headers, rows) =
            parse_csv("name,note\r\nalice,\"hello, world\"\r\nbob,\"say \"\"hi\"\"\"\r\n").unwrap();
        assert_eq!(headers, vec!["name", "note"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["note"], "hello, world");
        assert_eq!(rows[1]["note"], "say \"hi\"");
    }

    #[tokio::test]
    async fn queries_toml_and_csv_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let output = DataQuery
            .execute(
                &context(dir.path()),
                json!({"path": "Cargo.toml", "query": "package.version"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("0.1.0"));

        std::fs::write(
            dir.path().join("data.csv"),
            "name,score\nalice,10\nbob,20\n",
        )
        .unwrap();
        let output = DataQuery
            .execute(
                &context(dir.path()),
                json!({"path": "data.csv", "query": "rows[*].name"}),
            )
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("alice"));
        assert!(output.content.contains("bob"));
    }
}

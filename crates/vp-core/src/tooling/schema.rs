//! Bounded tool-argument checks and recovery hints.
//!
//! This is a usability guard, not an authorization boundary. It catches the
//! common shape mistakes a model makes (missing required field, wrong type,
//! invalid enum value, unknown field on a closed schema) before a tool runs.
//! Every tool still validates its own arguments after dispatch, and the normal
//! contract, approval, sandbox, effect, and budget gates are unchanged.

use anyhow::{Result, bail};
use serde_json::Value;

/// Maximum nesting depth checked. Provider tool arguments are shallow; a
/// deeper payload is rejected rather than traversed without a bound.
pub(crate) const MAX_ARGUMENT_DEPTH: usize = 8;

/// Minimal typed checking against the advertised JSON schema.
///
/// Supported keywords are `type`, `enum`, `required`, `properties`,
/// `additionalProperties: false`, `items`, `minItems`, `maxItems`,
/// `minProperties`, `maxProperties`, `minLength`, `maxLength`, `minimum`,
/// `maximum`, and numeric `exclusiveMinimum`/`exclusiveMaximum`. Unknown
/// keywords are ignored so this stays compatible with richer MCP schemas.
pub(crate) fn validate_arguments(schema: &Value, value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_ARGUMENT_DEPTH {
        bail!("argument nesting exceeds {MAX_ARGUMENT_DEPTH} levels");
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.iter().any(|option| option == value)
    {
        bail!("value is not one of the allowed enum values");
    }
    let Some(kind) = schema.get("type") else {
        return validate_common_keywords(schema, value, depth);
    };
    let allowed = match kind {
        Value::String(kind) => vec![kind.as_str()],
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !allowed.is_empty() && !allowed.iter().any(|kind| type_matches(kind, value)) {
        bail!(
            "expected {}, got {}",
            allowed.join(" or "),
            json_type(value)
        );
    }
    if value.is_object() {
        let object = value.as_object().expect("object");
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    bail!("missing required field `{name}`");
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        if schema.get("additionalProperties") == Some(&Value::Bool(false))
            && let Some(properties) = properties
        {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    bail!("unknown field `{key}`");
                }
            }
        }
        if let Some(min) = schema.get("minProperties").and_then(Value::as_u64)
            && (object.len() as u64) < min
        {
            bail!("object has fewer than minProperties {min}");
        }
        if let Some(max) = schema.get("maxProperties").and_then(Value::as_u64)
            && object.len() as u64 > max
        {
            bail!("object has more than maxProperties {max}");
        }
        if let Some(properties) = properties {
            for (key, item) in object {
                if let Some(property) = properties.get(key) {
                    validate_arguments(property, item, depth + 1)
                        .map_err(|error| anyhow::anyhow!("field `{key}`: {error}"))?;
                }
            }
        }
    }
    if value.is_array() {
        let items = value.as_array().expect("array");
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
            && (items.len() as u64) < min
        {
            bail!("array has fewer than minItems {min}");
        }
        if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
            && items.len() as u64 > max
        {
            bail!("array exceeds maxItems {max}");
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items.iter().take(64) {
                validate_arguments(item_schema, item, depth + 1)?;
            }
        }
    }
    validate_common_keywords(schema, value, depth)
}

fn validate_common_keywords(schema: &Value, value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_ARGUMENT_DEPTH {
        bail!("argument nesting exceeds {MAX_ARGUMENT_DEPTH} levels");
    }
    if let Some(text) = value.as_str() {
        if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
            && text.chars().count() as u64 > max
        {
            bail!("string exceeds maxLength {max}");
        }
        if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
            && (text.chars().count() as u64) < min
        {
            bail!("string is shorter than minLength {min}");
        }
    }
    if let Some(number) = value.as_f64() {
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && number > max
        {
            bail!("number exceeds maximum {max}");
        }
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && number < min
        {
            bail!("number is below minimum {min}");
        }
        if let Some(min) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
            && number <= min
        {
            bail!("number must be greater than {min}");
        }
        if let Some(max) = schema.get("exclusiveMaximum").and_then(Value::as_f64)
            && number >= max
        {
            bail!("number must be less than {max}");
        }
    }
    Ok(())
}

fn type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64()
                || value.is_u64()
                || value.as_f64().is_some_and(|value| value.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Actionable hint for a failed argument check. It names the correct fields for
/// the tools that are most often confused and otherwise points at the schema.
pub(crate) fn validation_hint(tool: &str, reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if tool == "run_project_task"
        && (lower.contains("unknown field `args`")
            || lower.contains("missing required field `kind`"))
    {
        return "run_project_task takes kind (build|test|lint|format|typecheck|check|dev|custom), package, filter, paths, command (only for kind=custom), cache, timeout_secs, and dry_run. It has no args field; narrow with package/filter/paths, or use kind=custom plus command for an exact command.".to_string();
    }
    if tool == "read_tool_output" && lower.contains("missing required field `handle`") {
        return "read_tool_output requires handle from the [output truncated] marker (output_ref is accepted as an alias), plus optional offset, limit, max_bytes, from_end, and pattern. It has no before/after fields.".to_string();
    }
    if tool == "read_file" && lower.contains("missing required field `path`") {
        return "read_file reads one file: pass path relative to the worktree. For two or more files use read_files with files:[{path,offset?,limit?}].".to_string();
    }
    if tool == "read_files" && lower.contains("missing required field `files`") {
        return "read_files requires files: an array of {path,offset?,limit?} objects (at most 20). For one file use read_file; there is no max_bytes field.".to_string();
    }
    if tool == "edit_file"
        && (lower.contains("missing required field `old_string`")
            || lower.contains("missing required field `new_string`")
            || lower.contains("missing required field `path`"))
    {
        return "edit_file requires path, old_string, and new_string. Read the file first and replace exact text. For a new file or complete rewrite use write_file.".to_string();
    }
    if tool == "write_file" && lower.contains("missing required field `content`") {
        return "write_file requires path and content. For a targeted change to an existing file use edit_file; for large files split content into <=48 KiB chunks and use append:true after the first call.".to_string();
    }
    if tool == "tool_help" && lower.contains("unknown field `format`") {
        return "tool_help has no format field; it always returns text plus structured recipes. Pass recipe, error_code, include_error_codes, or max_chars.".to_string();
    }
    if tool == "submit_team_plan" {
        if lower.contains("`department`") {
            return "Every task requires department set to one of departments[].name in the same plan, plus id, title, kind, and acceptance.".to_string();
        }
        if lower.contains("`departments`") {
            return "submit_team_plan requires at least one department with name, manager_title, profile, and objective; tasks reference department by exact name.".to_string();
        }
        if lower.contains("`acceptance`") {
            return "Every task requires a non-empty acceptance array of 1-8 concrete items (each at most 300 characters).".to_string();
        }
        if lower.contains("`kind`") {
            return "Every task requires kind, such as coding, research, verification, or review. A coding task needs profile=\"coding\" and a non-empty write_scope; verification is read-only with an empty write_scope.".to_string();
        }
    }
    if tool == "submit_verdict" && (lower.contains("`approved`") || lower.contains("`summary`")) {
        return "submit_verdict requires approved (boolean) and summary (concrete evidence and decision, max 2000 characters).".to_string();
    }
    if lower.contains("unknown field") {
        return format!(
            "Field names are exact and snake_case. Remove or rename the unknown field, then retry against the advertised {tool} schema."
        );
    }
    if lower.contains("missing required field") {
        return format!(
            "Add the missing required field(s) using the exact names in the advertised {tool} schema. Do not substitute shell flags or positional arguments."
        );
    }
    if lower.contains("expected ")
        || lower.contains("enum")
        || lower.contains("minimum")
        || lower.contains("maximum")
        || lower.contains("maxlength")
        || lower.contains("minlength")
        || lower.contains("maxitems")
        || lower.contains("minitems")
    {
        return format!(
            "The reported value violates the advertised {tool} schema. Fix the field type, enum, count, or bound shown above, then retry; do not resend the same arguments."
        );
    }
    format!(
        "Fix the reported argument against the advertised {tool} schema, then retry. Use tool_help for a bounded recipe; do not repeat the same call unchanged."
    )
}

/// Hint for arguments that could not be parsed at all, including a provider
/// truncation. The provider still needs a tool result, so this never retries on
/// its own.
pub(crate) fn parse_failure_hint(tool: &str, reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    let truncated = lower.contains("eof while parsing")
        || lower.contains("unexpected end")
        || lower.contains("trailing characters")
        || lower.contains("expected value");
    if truncated {
        return match tool {
            "write_file" => "The tool arguments look truncated. write_file accepts at most 48 KiB per call: split content into smaller chunks and use append:true after the first call.".to_string(),
            "apply_patch" => "The patch arguments look truncated. Split the change into smaller per-file patches, or use edit_file for one exact replacement.".to_string(),
            _ => "The tool arguments look truncated by the provider output limit. Resend a smaller call or split the work; do not retry the identical payload.".to_string(),
        };
    }
    if lower.contains("must be a json object") {
        return "Tool arguments must be one JSON object with named fields. Wrap positional values under the advertised field names; do not pass a bare array or string.".to_string();
    }
    format!(
        "Tool arguments for {tool} must be valid JSON using the advertised field names. Check quotes, commas, and escaping; call tool_search for the schema instead of guessing."
    )
}

/// Bounded suggestion for an unknown tool name. It only reports names from the
/// current advertised surface; it never dispatches a replacement call.
pub(crate) fn unknown_tool_hint(requested: &str, available: &[&str]) -> String {
    let requested_norm = normalize_tool_name(requested);
    if requested_norm.is_empty() {
        return "No tool name was provided. Use an exact advertised tool name; call tool_search with a short description to inspect one.".to_string();
    }
    let mut scored = available
        .iter()
        .filter_map(|name| {
            let normalized = normalize_tool_name(name);
            let score = if alias_match(&requested_norm, &normalized) || normalized == requested_norm
            {
                0
            } else if normalized.starts_with(&requested_norm)
                || requested_norm.starts_with(&normalized)
            {
                1
            } else if normalized.contains(&requested_norm) || requested_norm.contains(&normalized) {
                2
            } else if edit_distance_at_most(&requested_norm, &normalized, 2) {
                3
            } else {
                return None;
            };
            Some((score, *name))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    scored.dedup_by(|left, right| left.1 == right.1);
    let suggestions = scored
        .into_iter()
        .take(4)
        .map(|(_, name)| name)
        .collect::<Vec<_>>();
    if suggestions.is_empty() {
        return format!(
            "No tool named '{requested}' is advertised. Use an exact name from the current tool list; shell commands such as cat, ls, or grep are not tool names. Call tool_search with a short description to inspect an available tool."
        );
    }
    format!(
        "No tool named '{requested}' is advertised. Closest advertised names: {}. Use the exact name; call tool_search {{\"query\":\"{requested}\"}} to inspect its schema.",
        suggestions.join(", ")
    )
}

/// Normalize a small, documented set of argument aliases before validation.
/// This never changes the tool's authority or adds a field the schema does not
/// accept; it only lets a natural result-field name reach the documented one.
pub(crate) fn normalize_argument_aliases(tool: &str, input: &mut Value) {
    if tool != "read_tool_output" {
        return;
    }
    let Some(object) = input.as_object_mut() else {
        return;
    };
    let handle_valid = object
        .get("handle")
        .and_then(Value::as_str)
        .is_some_and(|handle| !handle.trim().is_empty());
    if handle_valid {
        return;
    }
    if let Some(alias) = object
        .get("output_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .map(str::to_string)
    {
        object.insert("handle".to_string(), Value::String(alias));
    }
}

fn normalize_tool_name(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace(['-', ' ', '.'], "_")
}

fn alias_match(requested: &str, name: &str) -> bool {
    match requested {
        "bash" | "sh" | "zsh" | "shell_command" | "terminal" | "run" => name == "shell",
        "cat" | "head" | "tail" => matches!(name, "read_file" | "read_files" | "read_tool_output"),
        "ls" | "dir" | "list" => name == "list_dir",
        "find" | "rg" | "ripgrep" => matches!(name, "grep" | "find_files"),
        "sed" | "awk" | "patch" => matches!(name, "edit_file" | "apply_patch"),
        "pytest" | "cargo_test" | "run_tests" | "test" => name == "run_project_task",
        "git_diff" | "diff" => name == "git",
        _ => false,
    }
}

fn edit_distance_at_most(left: &str, right: &str, limit: usize) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len().abs_diff(right.len()) > limit {
        return false;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0usize; right.len() + 1];
    for (row, left_byte) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_byte) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_byte != right_byte);
            let insertion = current[column] + 1;
            let deletion = previous[column + 1] + 1;
            current[column + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()] <= limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn required_fields_types_and_enums_are_checked_with_context() {
        let schema = json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string", "minLength": 1},
                "mode": {"type": "string", "enum": ["read", "write"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10}
            },
            "additionalProperties": false
        });
        assert!(validate_arguments(&schema, &json!({"path": "a.rs"}), 0).is_ok());
        assert!(validate_arguments(&schema, &json!({"mode": "read"}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({"path": 1}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "a.rs", "mode": "other"}), 0).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "a.rs", "extra": true}), 0).is_err());
        let error = validate_arguments(&schema, &json!({"path": "a.rs", "limit": 0}), 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `limit`"), "{error}");
    }

    #[test]
    fn array_bounds_and_item_shapes_are_checked() {
        let schema = json!({
            "type": "object",
            "required": ["files"],
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {
                        "type": "object",
                        "required": ["path"],
                        "properties": {"path": {"type": "string"}}
                    }
                }
            }
        });
        assert!(validate_arguments(&schema, &json!({"files": [{"path": "a"}]}), 0).is_ok());
        assert!(validate_arguments(&schema, &json!({"files": []}), 0).is_err());
        assert!(
            validate_arguments(
                &schema,
                &json!({"files": [{"path": "a"}, {"path": "b"}, {"path": "c"}]}),
                0
            )
            .is_err()
        );
        assert!(validate_arguments(&schema, &json!({"files": [{}]}), 0).is_err());
    }

    #[test]
    fn hints_name_the_confused_tools() {
        assert!(
            validation_hint("read_files", "missing required field `files`").contains("read_file")
        );
        assert!(
            validation_hint("run_project_task", "unknown field `args`").contains("no args field")
        );
        assert!(validation_hint("tool_help", "unknown field `format`").contains("no format field"));
        assert!(
            validation_hint("submit_team_plan", "missing required field `department`")
                .contains("departments[].name")
        );
        assert!(parse_failure_hint("write_file", "EOF while parsing a value").contains("48 KiB"));
        assert!(
            parse_failure_hint("read_file", "tool arguments must be a JSON object")
                .contains("JSON object")
        );
    }

    #[test]
    fn read_tool_output_accepts_the_output_ref_alias_before_validation() {
        let mut input = json!({"output_ref": "out_abc", "limit": 50});
        normalize_argument_aliases("read_tool_output", &mut input);
        assert_eq!(input["handle"], "out_abc");
        let schema = json!({
            "type": "object",
            "required": ["handle"],
            "properties": {
                "handle": {"type": "string"},
                "output_ref": {"type": "string"},
                "limit": {"type": "integer"}
            },
            "additionalProperties": false
        });
        assert!(validate_arguments(&schema, &input, 0).is_ok());
        let mut explicit = json!({"handle": "out_primary", "output_ref": "out_alias"});
        normalize_argument_aliases("read_tool_output", &mut explicit);
        assert_eq!(explicit["handle"], "out_primary");
        let mut null_handle = json!({"handle": null, "output_ref": "out_alias"});
        normalize_argument_aliases("read_tool_output", &mut null_handle);
        assert_eq!(null_handle["handle"], "out_alias");
    }

    #[test]
    fn unknown_tool_hints_are_bounded_and_deterministic() {
        let available = [
            "read_file",
            "read_files",
            "read_tool_output",
            "write_file",
            "edit_file",
            "shell",
            "run_project_task",
            "list_dir",
            "grep",
        ];
        let read = unknown_tool_hint("read", &available);
        assert!(read.contains("read_file"), "{read}");
        assert!(read.contains("tool_search"), "{read}");
        let bash = unknown_tool_hint("bash", &available);
        assert!(bash.contains("shell"), "{bash}");
        let pytest = unknown_tool_hint("pytest", &available);
        assert!(pytest.contains("run_project_task"), "{pytest}");
        let nonsense = unknown_tool_hint("zzzzz", &available);
        assert!(nonsense.contains("No tool named 'zzzzz'"));
        assert!(nonsense.len() < 600);
    }
}

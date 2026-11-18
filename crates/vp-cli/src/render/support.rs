//! Shared formatting helpers used across human renderers.

use super::*;

pub(crate) fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Short component-hash display for cache status/plan output. Full hashes stay
/// available in `--json`; the human view only needs a stable visual prefix.
pub(crate) fn short_hash(value: &Value) -> String {
    value
        .as_str()
        .map(|hash| hash.chars().take(12).collect::<String>() + "…")
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub(crate) fn print_program_line(program: &Value) {
    let id = program["id"].as_str().unwrap_or("?");
    let status = program["status"].as_str().unwrap_or("?");
    let goal = program["goal"].as_str().unwrap_or("");
    println!("{id}  {status:<10}  {goal}");
    let mut budget = Vec::new();
    if let Some(tokens) = program["token_budget"].as_u64() {
        budget.push(format!("{tokens} tokens"));
    }
    if let Some(cost) = program["cost_budget_usd"].as_f64() {
        budget.push(format!("${cost:.2}"));
    }
    if !budget.is_empty() {
        println!("  budget {}", budget.join(" / "));
    }
    if let Some(reason) = program["reason"].as_str() {
        println!("  {reason}");
    }
}

/// Human rendering for the durable context checkpoint surface. Returns
/// `None` for every other RPC shape so the generic printer owns them.
pub(crate) fn tool_activity_csv(value: &Value) -> String {
    const COLUMNS: &[&str] = &[
        "seq",
        "created_at",
        "tool_call_id",
        "name",
        "status",
        "error_code",
        "retryable",
        "duration_ms",
        "output_bytes",
        "truncated",
        "approval_risk",
        "approval_mode",
        "approval_wait_ms",
        "hint",
    ];
    let mut output = COLUMNS.join(",");
    output.push('\n');
    let Some(entries) = value.get("entries").and_then(Value::as_array) else {
        return output;
    };
    for entry in entries {
        let status = if entry["is_error"].as_bool().unwrap_or(false) {
            entry["error_code"].as_str().unwrap_or("TOOL_FAILED")
        } else {
            "ok"
        };
        let row = [
            entry["seq"].as_u64().unwrap_or(0).to_string(),
            entry["created_at"].as_str().unwrap_or("").to_string(),
            entry["tool_call_id"].as_str().unwrap_or("").to_string(),
            entry["name"].as_str().unwrap_or("").to_string(),
            status.to_string(),
            entry["error_code"].as_str().unwrap_or("").to_string(),
            entry["retryable"].as_bool().unwrap_or(false).to_string(),
            entry["duration_ms"].as_u64().unwrap_or(0).to_string(),
            entry["output_bytes"].as_u64().unwrap_or(0).to_string(),
            entry["truncated"].as_bool().unwrap_or(false).to_string(),
            entry["approval_risk"].as_str().unwrap_or("").to_string(),
            entry["approval_mode"].as_str().unwrap_or("").to_string(),
            entry["approval_wait_ms"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_default(),
            entry["hint"].as_str().unwrap_or("").to_string(),
        ];
        output.push_str(
            &row.iter()
                .map(|cell| csv_cell(cell))
                .collect::<Vec<_>>()
                .join(","),
        );
        output.push('\n');
    }
    output
}

pub(crate) fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(crate) fn filtered_tool_codes(
    category: Option<&str>,
    retryable: Option<bool>,
) -> Vec<vp_core::tooling::errors::ToolErrorCatalogEntry> {
    let mut entries = vp_core::tooling::errors::catalog();
    if let Some(category) = category {
        let category = category.trim().to_ascii_lowercase();
        entries.retain(|entry| entry.category == category);
    }
    if let Some(retryable) = retryable {
        entries.retain(|entry| entry.retryable == retryable);
    }
    entries.sort_by(|left, right| {
        left.category
            .cmp(right.category)
            .then_with(|| left.code.as_str().cmp(right.code.as_str()))
    });
    entries
}

pub(crate) fn bounded_line(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect::<String>() + "..."
    }
}

pub(crate) fn short_hash_str(value: &str) -> String {
    value.chars().take(12).collect()
}

pub(crate) fn short_optional_hash(value: Option<&str>) -> String {
    value
        .map(|value| value.chars().take(12).collect::<String>())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

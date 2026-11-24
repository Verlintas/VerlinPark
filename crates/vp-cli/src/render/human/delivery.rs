//! Human-readable explicit partial-delivery and delivery-status views.

use super::*;

/// Render one of the two delivery result shapes:
///
/// - `SessionDeliveryResult { session_id, partial, readiness }`
/// - `PartialDeliveryResult { session_id, record, readiness }`
///
/// Returns `None` for unrelated JSON so the shared renderer can keep trying.
pub(crate) fn delivery_lines(value: &Value) -> Option<Vec<String>> {
    // Both result shapes require session_id + readiness. The tight gate avoids
    // stealing unrelated objects that happen to contain a `record` key.
    value.get("session_id").and_then(Value::as_str)?;
    let readiness = value.get("readiness")?;
    if let Some(record) = value.get("record").filter(|record| record.is_object()) {
        let mut lines = Vec::new();
        lines.push(format!(
            "partial delivery: {}",
            record.get("id").and_then(Value::as_str).unwrap_or("?")
        ));
        push_record_lines(&mut lines, record);
        return Some(lines);
    }
    let partial = value.get("partial")?;
    if partial.is_null() {
        let mut lines = vec!["delivery: no partial delivery recorded".to_string()];
        lines.push(format!(
            "  readiness: level={}",
            readiness
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
        return Some(lines);
    }
    if !partial.is_object() {
        return None;
    }
    let mut lines = vec![format!(
        "partial delivery: {}",
        partial.get("id").and_then(Value::as_str).unwrap_or("?")
    )];
    push_record_lines(&mut lines, partial);
    Some(lines)
}

fn push_record_lines(lines: &mut Vec<String>, record: &Value) {
    push_text(
        lines,
        "recorded_at",
        record.get("recorded_at").and_then(Value::as_str),
    );
    push_text(
        lines,
        "recorded_by",
        record.get("recorded_by").and_then(Value::as_str),
    );
    push_text(
        lines,
        "summary",
        record.get("summary").and_then(Value::as_str),
    );
    push_text(
        lines,
        "readiness_level",
        record.get("readiness_level").and_then(Value::as_str),
    );
    let commit = record
        .get("latest_commit")
        .and_then(Value::as_str)
        .unwrap_or("none");
    lines.push(format!("  commit: {commit}"));
    lines.push(format!(
        "  uncommitted: {}",
        record
            .get("uncommitted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    ));
    push_list(lines, "changed_paths", record.get("changed_paths"));
    push_list(lines, "remaining_work", record.get("remaining_work"));
    push_list(lines, "blockers", record.get("blockers"));
}

fn push_text(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("  {label}: {value}"));
    }
}

fn push_list(lines: &mut Vec<String>, label: &str, value: Option<&Value>) {
    let Some(items) = value
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    else {
        return;
    };
    lines.push(format!("  {label}:"));
    for item in items.iter().filter_map(Value::as_str) {
        lines.push(format!("    {item}"));
    }
}

pub(crate) fn print(value: &Value) -> bool {
    let Some(lines) = delivery_lines(value) else {
        return false;
    };
    for line in lines {
        println!("{line}");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delivery_without_a_record_states_so_and_shows_readiness() {
        let value = json!({
            "session_id": "ses-1",
            "partial": null,
            "readiness": {
                "session_id": "ses-1",
                "level": "committed",
                "shippable": false,
                "mergeable": false,
                "generated_at": "2026-11-24T00:00:00Z"
            }
        });
        let rendered = delivery_lines(&value).unwrap().join("\n");
        assert!(rendered.contains("no partial delivery recorded"));
        assert!(rendered.contains("level=committed"));
    }

    #[test]
    fn recorded_delivery_shows_operator_fields_without_file_contents() {
        let value = json!({
            "session_id": "ses-1",
            "record": {
                "schema": "verlinpark.partial_delivery/v1",
                "id": "pd-1",
                "session_id": "ses-1",
                "project_id": "prj-1",
                "recorded_at": "2026-11-24T00:00:00Z",
                "recorded_by": "operator",
                "summary": "stopped after the parser",
                "remaining_work": ["wire the CLI"],
                "readiness_level": "dirty",
                "latest_commit": null,
                "changed_paths": ["src/parser.rs"],
                "blockers": ["1 pending approval(s)"],
                "uncommitted": true
            },
            "readiness": {
                "session_id": "ses-1",
                "level": "dirty",
                "shippable": false,
                "mergeable": false,
                "generated_at": "2026-11-24T00:00:00Z"
            }
        });
        let rendered = delivery_lines(&value).unwrap().join("\n");
        assert!(rendered.contains("pd-1"));
        assert!(rendered.contains("recorded_at: 2026-11-24T00:00:00Z"));
        assert!(rendered.contains("recorded_by: operator"));
        assert!(rendered.contains("summary: stopped after the parser"));
        assert!(rendered.contains("readiness_level: dirty"));
        assert!(rendered.contains("commit: none"));
        assert!(rendered.contains("uncommitted: true"));
        assert!(rendered.contains("src/parser.rs"));
        assert!(rendered.contains("wire the CLI"));
        assert!(rendered.contains("1 pending approval(s)"));

        let no_record = json!({
            "session_id": "ses-1",
            "partial": null,
            "readiness": {"level": "none"}
        });
        assert!(
            delivery_lines(&no_record)
                .unwrap()
                .join("\n")
                .contains("no partial delivery recorded")
        );
        assert!(delivery_lines(&json!({"unrelated": true})).is_none());
        assert!(
            delivery_lines(&json!({
                "record": {"id": "pd-unrelated"},
                "readiness": {"level": "none"}
            }))
            .is_none(),
            "a record without session_id must not be treated as a delivery result"
        );
    }
}

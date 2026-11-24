//! Human-readable rescue/allocation preview and grant results.

use super::*;

/// Render `RescueStatus` and `RescueResult`. Both carry a `session_id` and a
/// status object (`RescueResult` nests it under `status`). Returns `None` for
/// unrelated JSON so the shared renderer can keep trying.
pub(crate) fn rescue_lines(value: &Value) -> Option<Vec<String>> {
    let session_id = value.get("session_id").and_then(Value::as_str)?;
    let status = match value.get("status").filter(|status| status.is_object()) {
        Some(status) => status,
        None => value,
    };
    if status.get("eligible").is_none() && status.get("active").is_none() {
        return None;
    }

    let mut lines = vec![format!("session: {session_id}")];
    lines.push(format!(
        "rescue: active={} eligible={} reason={}",
        status
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status
            .get("eligible")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    ));

    if let Some(grant) = value
        .get("grant")
        .filter(|grant| grant.is_object())
        .or_else(|| status.get("grant").filter(|grant| grant.is_object()))
    {
        lines.push(format!(
            "  grant: {}",
            grant.get("id").and_then(Value::as_str).unwrap_or("?")
        ));
        push_text(
            &mut lines,
            "expires_at",
            grant.get("expires_at").and_then(Value::as_str),
        );
        push_text(
            &mut lines,
            "recorded_by",
            grant.get("recorded_by").and_then(Value::as_str),
        );
        push_text(
            &mut lines,
            "reason",
            grant.get("reason").and_then(Value::as_str),
        );
        push_text(
            &mut lines,
            "readiness_level",
            grant.get("readiness_level").and_then(Value::as_str),
        );
        push_text(
            &mut lines,
            "latest_commit",
            grant.get("latest_commit").and_then(Value::as_str),
        );
    }
    push_text(
        &mut lines,
        "used_at",
        status.get("used_at").and_then(Value::as_str),
    );
    push_text(
        &mut lines,
        "used_grant_id",
        status.get("used_grant_id").and_then(Value::as_str),
    );

    if let Some(readiness) = status.get("readiness").filter(|value| value.is_object()) {
        lines.push(format!(
            "  readiness: level={} shippable={} mergeable={}",
            readiness
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            readiness
                .get("shippable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            readiness
                .get("mergeable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ));
        push_text(
            &mut lines,
            "latest_commit",
            readiness.get("latest_commit").and_then(Value::as_str),
        );
    }
    lines.push(format!(
        "  no_progress_turns: {}",
        status
            .get("no_progress_turns")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    ));

    if let Some(reserve) = status.get("reserve").filter(|value| value.is_object()) {
        lines.push(format!(
            "  reserve: ratio={}",
            optional_number(reserve.get("completion_reserve_ratio"))
        ));
        lines.push(format!(
            "    remaining: tokens={} cost_usd={} secs={}",
            optional_number(reserve.get("remaining_tokens")),
            optional_number(reserve.get("remaining_cost_usd")),
            optional_number(reserve.get("remaining_secs"))
        ));
        lines.push(format!(
            "    hard_caps: tokens={} cost_usd={} secs={}",
            optional_number(reserve.get("hard_token_cap")),
            optional_number(reserve.get("hard_cost_cap_usd")),
            optional_number(reserve.get("hard_wall_clock_secs"))
        ));
    }

    push_candidates(&mut lines, "near_completion", status.get("near_completion"));
    push_candidates(&mut lines, "looping", status.get("looping"));
    push_list(&mut lines, "recommendations", status.get("recommendations"));
    Some(lines)
}

fn optional_number(value: Option<&Value>) -> String {
    match value {
        Some(Value::Null) | None => "none".to_string(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) if text.trim().is_empty() => "none".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn push_text(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("  {label}: {value}"));
    }
}

fn push_candidates(lines: &mut Vec<String>, label: &str, value: Option<&Value>) {
    let Some(items) = value
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    else {
        return;
    };
    lines.push(format!("  {label}:"));
    for item in items.iter().filter(|item| item.is_object()) {
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or("?");
        let id = item.get("id").and_then(Value::as_str).unwrap_or("?");
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let commit = item
            .get("commit")
            .and_then(Value::as_str)
            .map(|commit| format!(" commit={commit}"))
            .unwrap_or_default();
        let detail = item
            .get("detail")
            .and_then(Value::as_str)
            .filter(|detail| !detail.trim().is_empty())
            .map(|detail| format!(" {detail}"))
            .unwrap_or_default();
        lines.push(format!("    {kind} {id} {status}{commit}{detail}"));
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
    let Some(lines) = rescue_lines(value) else {
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
    fn rescue_status_renders_reserve_candidates_and_recommendations() {
        let value = json!({
            "session_id": "ses-1",
            "active": false,
            "grant": null,
            "used_at": null,
            "used_grant_id": null,
            "eligible": true,
            "reason": "eligible",
            "readiness": {
                "session_id": "ses-1",
                "level": "committed",
                "shippable": false,
                "mergeable": false,
                "latest_commit": "abc123",
                "generated_at": "2026-11-24T00:00:00Z"
            },
            "no_progress_turns": 0,
            "near_completion": [{
                "kind": "merge_queue",
                "id": "mq-1",
                "status": "queued",
                "detail": "task T1: awaiting integration",
                "commit": "abc123"
            }],
            "looping": [],
            "recommendations": ["rescue_reserve", "reallocate_attention_to_near_completion"],
            "reserve": {
                "completion_reserve_ratio": 0.2,
                "remaining_tokens": 150,
                "remaining_cost_usd": null,
                "remaining_secs": 90,
                "hard_token_cap": 1000,
                "hard_cost_cap_usd": null,
                "hard_wall_clock_secs": 600
            },
            "generated_at": "2026-11-24T00:00:00Z"
        });
        let rendered = rescue_lines(&value).unwrap().join("\n");
        assert!(rendered.contains("active=false eligible=true reason=eligible"));
        assert!(rendered.contains("readiness: level=committed"));
        assert!(rendered.contains("no_progress_turns: 0"));
        assert!(rendered.contains("ratio=0.2"));
        assert!(rendered.contains("tokens=150"));
        assert!(rendered.contains("cost_usd=none"));
        assert!(rendered.contains("hard_caps: tokens=1000"));
        assert!(rendered.contains("merge_queue mq-1 queued commit=abc123"));
        assert!(rendered.contains("rescue_reserve"));
        assert!(!rendered.contains("src/"));
    }

    #[test]
    fn rescue_result_renders_the_grant_and_nested_status() {
        let value = json!({
            "session_id": "ses-1",
            "grant": {
                "schema": "verlinpark.rescue_grant/v1",
                "id": "rs-1",
                "session_id": "ses-1",
                "project_id": "prj-1",
                "recorded_at": "2026-11-24T00:00:00Z",
                "expires_at": "2026-11-24T00:30:00Z",
                "recorded_by": "operator",
                "reason": "finish verification",
                "readiness_level": "verified",
                "latest_commit": "abc123",
                "no_progress_turns": 0
            },
            "status": {
                "session_id": "ses-1",
                "active": true,
                "grant": {"id": "rs-1"},
                "eligible": false,
                "reason": "active_grant",
                "readiness": {"level": "verified", "latest_commit": "abc123"},
                "no_progress_turns": 0,
                "near_completion": [],
                "looping": [],
                "recommendations": ["rescue_active"],
                "reserve": {"completion_reserve_ratio": 0.2},
                "generated_at": "2026-11-24T00:00:00Z"
            }
        });
        let rendered = rescue_lines(&value).unwrap().join("\n");
        assert!(rendered.contains("grant: rs-1"));
        assert!(rendered.contains("expires_at: 2026-11-24T00:30:00Z"));
        assert!(rendered.contains("recorded_by: operator"));
        assert!(rendered.contains("reason: finish verification"));
        assert!(rendered.contains("active=true eligible=false reason=active_grant"));
        assert!(rendered.contains("rescue_active"));
        assert!(rescue_lines(&json!({"unrelated": true})).is_none());
    }
}

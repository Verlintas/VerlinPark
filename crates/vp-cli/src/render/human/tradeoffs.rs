//! Human-readable R4 trade-off and quality view.

use super::*;

pub(crate) fn print(value: &Value) -> bool {
    let Some(lines) = tradeoff_lines(value) else {
        return false;
    };
    for line in lines {
        println!("{line}");
    }
    true
}

pub(crate) fn tradeoff_lines(value: &Value) -> Option<Vec<String>> {
    let profiles = value.get("profiles").and_then(Value::as_array)?;
    let quality = value.get("quality").filter(|value| value.is_object())?;
    let confidence = value.get("confidence").filter(|value| value.is_object())?;
    let session_id = value.get("session_id").and_then(Value::as_str)?;

    let mut lines = vec![format!(
        "tradeoffs: session={session_id} current={} generated={}",
        value
            .get("current_profile")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("generated_at")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )];
    lines.push(format!(
        "  active reserve: {}% · source {} (profile rows are documented posture)",
        value
            .get("effective_completion_reserve_ratio")
            .and_then(Value::as_f64)
            .map(|ratio| format!("{:.0}", ratio * 100.0))
            .unwrap_or_else(|| "?".to_string()),
        value
            .get("reserve_source")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    ));
    lines.push(format!(
        "  quality: readiness={} reviewability={} tasks={}/{} merged_entries={}",
        quality
            .get("readiness_level")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        quality
            .get("reviewability")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        optional_number(quality.get("completed_tasks")),
        optional_number(quality.get("total_tasks")),
        optional_number(quality.get("merged_entries"))
    ));
    for (label, key) in [
        ("commit", "latest_commit"),
        ("test", "latest_test"),
        ("verifier", "verifier"),
        ("merge", "merge"),
    ] {
        if let Some(value) = quality
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            lines.push(format!("    {label}: {value}"));
        }
    }
    push_list(&mut lines, "reasons", quality.get("reasons"), 8);
    push_list(&mut lines, "blockers", quality.get("blockers"), 8);
    lines.push(format!(
        "  confidence: level={} score={} samples={}",
        confidence
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        optional_number(confidence.get("score")),
        optional_number(confidence.get("sample_calls"))
    ));
    for reason in confidence
        .get("reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(6)
    {
        lines.push(format!("    {reason}"));
    }
    lines.push("  profiles:".to_string());
    for profile in profiles
        .iter()
        .take(5)
        .filter(|profile| profile.is_object())
    {
        lines.push(format!(
            "    {}{} goal={} reserve={} turns={} cost=${}",
            profile
                .get("profile")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            if profile
                .get("recommended")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                " *"
            } else {
                ""
            },
            profile
                .get("primary_goal")
                .and_then(Value::as_str)
                .unwrap_or(""),
            optional_number(profile.get("completion_reserve_ratio")),
            optional_number(profile.get("projected_turns")),
            optional_number(profile.get("projected_cost_usd"))
        ));
        if let Some(attention) = profile.get("attention_posture").and_then(Value::as_str) {
            lines.push(format!("      attention: {attention}"));
        }
        if let Some(quality) = profile.get("quality_posture").and_then(Value::as_str) {
            lines.push(format!("      quality: {quality}"));
        }
    }
    push_list(
        &mut lines,
        "recommendations",
        value.get("recommendations"),
        12,
    );
    push_list(&mut lines, "warnings", value.get("warnings"), 8);
    Some(lines)
}

fn push_list(lines: &mut Vec<String>, label: &str, value: Option<&Value>, limit: usize) {
    let Some(items) = value
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    else {
        return;
    };
    lines.push(format!("  {label}:"));
    for item in items.iter().take(limit) {
        if let Some(item) = item.as_str() {
            lines.push(format!("    {item}"));
        }
    }
    if items.len() > limit {
        lines.push(format!("    … {} more", items.len() - limit));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tradeoff_view_renders_profiles_quality_and_confidence() {
        let value = json!({
            "session_id": "ses-1",
            "generated_at": "2026-11-24T00:00:00Z",
            "current_profile": "balanced",
            "effective_completion_reserve_ratio": 0.2,
            "reserve_source": "project_policy",
            "profiles": [{
                "profile": "balanced",
                "primary_goal": "default",
                "completion_reserve_ratio": 0.2,
                "attention_posture": "moderate",
                "quality_posture": "tests + Verifier",
                "projected_turns": 6,
                "projected_cost_usd": 0.4,
                "recommended": true
            }, {
                "profile": "quality",
                "primary_goal": "trustworthy completion",
                "completion_reserve_ratio": 0.25,
                "attention_posture": "high tolerance",
                "quality_posture": "tests + Verifier + reviewability",
                "projected_turns": 6,
                "projected_cost_usd": 0.4,
                "recommended": false
            }],
            "quality": {
                "readiness_level": "committed",
                "latest_commit": "abc123",
                "latest_test": null,
                "verifier": null,
                "merge": null,
                "completed_tasks": 1,
                "total_tasks": 2,
                "merged_entries": 0,
                "changed_paths": ["src/lib.rs"],
                "blockers": [],
                "reviewability": "watch",
                "reasons": ["no_tests", "no_verifier"]
            },
            "confidence": {
                "level": "medium",
                "score": 0.55,
                "sample_calls": 3,
                "reasons": ["commit evidence", "no Verifier evidence"]
            },
            "recommendations": ["add_tests", "wait_for_verifier"],
            "warnings": ["No post-commit test evidence is recorded."]
        });
        let text = tradeoff_lines(&value).expect("tradeoff lines").join("\n");
        assert!(text.contains("tradeoffs: session=ses-1 current=balanced"));
        assert!(text.contains("active reserve: 20%"));
        assert!(text.contains("source project_policy"));
        assert!(text.contains("reviewability=watch"));
        assert!(text.contains("confidence: level=medium"));
        assert!(text.contains("balanced *"));
        assert!(text.contains("quality"));
        assert!(text.contains("reserve=0.25"));
        assert!(text.contains("add_tests"));
        assert!(text.contains("No post-commit test evidence"));
        assert!(tradeoff_lines(&json!({"session_id": "ses-1"})).is_none());
    }
}

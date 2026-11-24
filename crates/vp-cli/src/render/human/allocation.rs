//! Human-readable R3 allocation preview.

use super::*;

pub(crate) fn print(value: &Value) -> bool {
    let Some(lines) = allocation_lines(value) else {
        return false;
    };
    for line in lines {
        println!("{line}");
    }
    true
}

pub(crate) fn allocation_lines(value: &Value) -> Option<Vec<String>> {
    let envelope = value.get("envelope").filter(|value| value.is_object())?;
    let feasibility = value.get("feasibility").filter(|value| value.is_object())?;
    let tasks = value.get("tasks").and_then(Value::as_array)?;
    let session_id = value.get("session_id").and_then(Value::as_str)?;

    let mut lines = vec![format!(
        "allocation: session={session_id} profile={} generated={}",
        value
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("generated_at")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )];
    lines.push(format!(
        "  envelope: tokens={} spent={} remaining={} cost=${} spent=${} remaining=${} time={}s remaining={} reserve_ratio={}",
        optional_number(envelope.get("token_budget")),
        optional_number(envelope.get("spent_tokens")),
        optional_number(envelope.get("remaining_tokens")),
        optional_number(envelope.get("cost_budget_usd")),
        optional_number(envelope.get("spent_cost_usd")),
        optional_number(envelope.get("remaining_cost_usd")),
        optional_number(envelope.get("time_budget_secs")),
        optional_number(envelope.get("remaining_secs")),
        optional_number(envelope.get("completion_reserve_ratio"))
    ));
    lines.push(format!(
        "  feasibility: feasible={} reason={} source={} p50={} p90={} cost_p50=${} cost_p90=${} samples={} reserve={} headroom={} critical_path={}",
        feasibility
            .get("feasible")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        feasibility
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        feasibility
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        optional_number(feasibility.get("p50_tokens")),
        optional_number(feasibility.get("p90_tokens")),
        optional_number(feasibility.get("p50_cost_usd")),
        optional_number(feasibility.get("p90_cost_usd")),
        optional_number(feasibility.get("sample_calls")),
        optional_number(feasibility.get("reserve_tokens")),
        optional_number(feasibility.get("headroom_tokens")),
        feasibility
            .get("critical_path_tasks")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    ));
    let truncated = value
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    lines.push(format!(
        "  tasks: {} shown{}",
        tasks.len().min(10),
        if truncated { " (truncated)" } else { "" }
    ));
    for task in tasks.iter().take(10) {
        let runtime = if task
            .get("envelope_reallocated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            format!(
                " runtime_tokens={} runtime_cost=${}",
                optional_number(task.get("effective_token_budget")),
                optional_number(task.get("effective_cost_budget_usd"))
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "    {} p{} {:<10} ready={} critical={} feasible={} source={} p50={} p90={} calls={}{} {}",
            task.get("task_id").and_then(Value::as_str).unwrap_or("?"),
            task.get("priority").and_then(Value::as_i64).unwrap_or(0),
            task.get("status").and_then(Value::as_str).unwrap_or("?"),
            task.get("ready").and_then(Value::as_bool).unwrap_or(false),
            task.get("critical_path")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            task.get("feasible")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            task.get("source").and_then(Value::as_str).unwrap_or("?"),
            optional_number(task.get("p50_tokens")),
            optional_number(task.get("p90_tokens")),
            optional_number(task.get("observed_calls")),
            runtime,
            task.get("reason").and_then(Value::as_str).unwrap_or("")
        ));
    }
    if tasks.len() > 10 {
        lines.push(format!("    … {} more task(s)", tasks.len() - 10));
    }
    push_recommendations(&mut lines, value.get("recommendations"));
    if let Some(meta) = value.get("metareasoning").filter(|value| value.is_object()) {
        lines.push(format!(
            "  metareasoning: diagnosis={} reason={} target={} no_progress={} stalled={} failed={} retrying={}",
            meta.get("diagnosis_status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            meta.get("diagnosis_reason")
                .and_then(Value::as_str)
                .unwrap_or("none"),
            meta.get("diagnosis_target")
                .and_then(Value::as_str)
                .unwrap_or("none"),
            optional_number(meta.get("no_progress_turns")),
            list_count(meta.get("stalled_tasks")),
            list_count(meta.get("failed_tasks")),
            list_count(meta.get("retrying_tasks"))
        ));
    }
    if let Some(calibration) = value.get("calibration").filter(|value| value.is_object()) {
        lines.push(format!(
            "  calibration: attention={} trust={} confidence={} pending={} interrupts={}/{} verifier_rejections={} failed={}",
            calibration
                .get("attention_recommendation")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            calibration
                .get("trust_recommendation")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            calibration
                .get("confidence")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            optional_number(calibration.get("pending_approvals")),
            optional_number(calibration.get("attention_interrupts")),
            optional_number(calibration.get("attention_cap")),
            optional_number(calibration.get("verifier_rejections")),
            optional_number(calibration.get("failed_tasks"))
        ));
        if let Some(reason) = calibration.get("reason").and_then(Value::as_str)
            && !reason.is_empty()
        {
            lines.push(format!("    {reason}"));
        }
    }
    Some(lines)
}

fn push_recommendations(lines: &mut Vec<String>, value: Option<&Value>) {
    let Some(items) = value
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    else {
        return;
    };
    lines.push("  recommendations:".to_string());
    for item in items.iter().take(12).filter(|item| item.is_object()) {
        lines.push(format!(
            "    {} ({}): {}{}",
            item.get("action").and_then(Value::as_str).unwrap_or("?"),
            item.get("confidence")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            item.get("reason").and_then(Value::as_str).unwrap_or(""),
            item.get("task_id")
                .and_then(Value::as_str)
                .map(|task_id| format!(" [task {task_id}]"))
                .unwrap_or_default()
        ));
    }
    if items.len() > 12 {
        lines.push(format!("    … {} more recommendation(s)", items.len() - 12));
    }
}

fn list_count(value: Option<&Value>) -> usize {
    value.and_then(Value::as_array).map(Vec::len).unwrap_or(0)
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
    fn allocation_preview_renders_bounded_tasks_and_never_matches_other_views() {
        let value = json!({
            "session_id": "ses-1",
            "generated_at": "2026-11-24T00:00:00Z",
            "profile": "balanced",
            "readiness": {"level": "committed"},
            "envelope": {
                "token_budget": 10000,
                "spent_tokens": 4000,
                "remaining_tokens": 6000,
                "cost_budget_usd": null,
                "spent_cost_usd": 0.0,
                "remaining_cost_usd": null,
                "time_budget_secs": null,
                "remaining_secs": null,
                "completion_reserve_ratio": 0.2
            },
            "feasibility": {
                "feasible": true,
                "reason": "feasible",
                "source": "session_actuals",
                "p50_tokens": 2000,
                "p90_tokens": 3000,
                "p50_cost_usd": null,
                "p90_cost_usd": null,
                "sample_calls": 3,
                "reserve_tokens": 2000,
                "headroom_tokens": 4000,
                "critical_path_tasks": ["T1"]
            },
            "tasks": [{
                "task_id": "T1",
                "title": "Build it",
                "department": "impl",
                "status": "pending",
                "priority": 5,
                "token_budget": 1000,
                "cost_budget_usd": null,
                "effective_token_budget": 4000,
                "effective_cost_budget_usd": null,
                "envelope_reallocated": true,
                "p50_tokens": 2000,
                "p90_tokens": 3000,
                "observed_calls": 3,
                "source": "session_actuals",
                "critical_path": true,
                "ready": true,
                "feasible": true,
                "reason": "within_envelope"
            }],
            "recommendations": [{
                "action": "continue",
                "reason": "fits",
                "confidence": "medium",
                "task_id": null
            }],
            "metareasoning": {
                "diagnosis_status": "no_failure",
                "diagnosis_reason": null,
                "diagnosis_target": null,
                "no_progress_turns": 0,
                "stalled_tasks": [],
                "failed_tasks": [],
                "retrying_tasks": [],
                "recommendations": []
            },
            "calibration": {
                "attention_recommendation": "batch_routine",
                "trust_recommendation": "standard",
                "reason": "no rejection evidence",
                "confidence": "medium",
                "pending_approvals": 0,
                "attention_interrupts": 0,
                "attention_cap": 20,
                "verifier_rejections": 0,
                "failed_tasks": 0
            },
            "truncated": false
        });
        let text = allocation_lines(&value)
            .expect("allocation preview lines")
            .join("\n");
        assert!(text.contains("allocation: session=ses-1 profile=balanced"));
        assert!(text.contains("reserve_ratio=0.2"));
        assert!(text.contains("feasibility: feasible=true reason=feasible"));
        assert!(text.contains("T1 p5 pending"));
        assert!(text.contains("critical=true"));
        assert!(text.contains("runtime_tokens=4000"));
        assert!(text.contains("continue (medium)"));
        assert!(text.contains("diagnosis=no_failure"));
        assert!(text.contains("batch_routine"));
        assert!(allocation_lines(&json!({"session_id": "ses-1"})).is_none());
    }
}

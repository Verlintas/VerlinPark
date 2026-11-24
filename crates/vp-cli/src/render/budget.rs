//! Human and CSV renderers for session budgets and the project auto policy.

use super::*;

fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

fn format_tokens(value: Option<u64>) -> String {
    value
        .map(group_thousands)
        .unwrap_or_else(|| "none".to_string())
}

fn format_usd(value: Option<f64>) -> String {
    value
        .map(|value| format!("${value:.4}"))
        .unwrap_or_else(|| "none".to_string())
}

fn format_duration(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "none".to_string();
    };
    if seconds == 0 {
        return "0s".to_string();
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if secs > 0 && days == 0 {
        parts.push(format!("{secs}s"));
    }
    parts.join(" ")
}

fn format_optional_text(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("none")
        .to_string()
}

fn status_object(value: &Value) -> Option<&Value> {
    let candidate = value.get("budget").unwrap_or(value);
    let has_mode = candidate
        .get("mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| !mode.is_empty());
    let has_forecast = candidate.get("forecast").is_some_and(Value::is_object);
    let has_session = candidate
        .get("session_id")
        .and_then(Value::as_str)
        .is_some_and(|session| !session.is_empty());
    (has_mode && has_forecast && has_session).then_some(candidate)
}

fn forecast_object(value: &Value) -> Option<&Value> {
    value
        .get("forecast")
        .filter(|forecast| forecast.is_object())
        .filter(|_| value.get("mode").is_none())
}

fn auto_policy_object(value: &Value) -> Option<&Value> {
    let policy = value.get("policy").filter(|policy| policy.is_object())?;
    let has_project = value
        .get("project_id")
        .and_then(Value::as_str)
        .is_some_and(|project| !project.is_empty());
    let looks_like_policy = policy.get("max_turns_per_run").is_some()
        || policy.get("no_progress_action").is_some()
        || policy.get("auto_tool_bundle").is_some();
    (has_project && looks_like_policy).then_some(policy)
}

fn project_run_policy_object(value: &Value) -> Option<&Value> {
    let policy = value.get("policy").filter(|policy| policy.is_object())?;
    let has_project = value
        .get("project_id")
        .and_then(Value::as_str)
        .is_some_and(|project| !project.is_empty());
    let looks_like_policy = policy.get("default_profile").is_some()
        && policy.get("completion_reserve_ratio").is_some()
        && policy.get("attention_cap").is_some();
    (has_project && looks_like_policy).then_some(policy)
}

fn status_lines(status: &Value, updated: bool) -> Vec<String> {
    let mode = status["mode"].as_str().unwrap_or("?");
    let unlimited = status["unlimited"].as_bool().unwrap_or(false);
    let mut lines = vec![format!(
        "session {} budget{}{}",
        status["session_id"].as_str().unwrap_or("?"),
        if updated { " updated" } else { "" },
        if unlimited {
            format!(" · mode {mode} (unlimited flag set)")
        } else {
            format!(" · mode {mode}")
        }
    )];
    lines.push(format!(
        "  effective   tokens {} · {} · {}",
        format_tokens(status["effective_token_budget"].as_u64()),
        format_usd(status["effective_cost_budget_usd"].as_f64()),
        format_duration(status["effective_time_budget_secs"].as_u64())
    ));
    lines.push(format!(
        "  hard caps   tokens {} · {} · {}",
        format_tokens(status["hard_token_cap"].as_u64()),
        format_usd(status["hard_cost_cap_usd"].as_f64()),
        format_duration(status["hard_wall_clock_secs"].as_u64())
    ));
    lines.push(format!(
        "  spent       {} tokens · {} · {}",
        group_thousands(status["spent_tokens"].as_u64().unwrap_or(0)),
        format_usd(Some(status["spent_cost_usd"].as_f64().unwrap_or(0.0))),
        format_duration(Some(status["elapsed_secs"].as_u64().unwrap_or(0)))
    ));
    lines.push(format!(
        "  remaining   {} tokens · {} · {}",
        format_tokens(status["remaining_tokens"].as_u64()),
        format_usd(status["remaining_cost_usd"].as_f64()),
        format_duration(status["remaining_secs"].as_u64())
    ));
    if let Some(forecast) = status.get("forecast").filter(|value| value.is_object()) {
        lines.push(format!(
            "  forecast    {} call(s) · avg {} in / {} out · {}/call · {}",
            forecast["sample_calls"].as_u64().unwrap_or(0),
            group_thousands(forecast["avg_input_tokens_per_call"].as_u64().unwrap_or(0)),
            group_thousands(forecast["avg_output_tokens_per_call"].as_u64().unwrap_or(0)),
            format_usd(Some(
                forecast["avg_cost_usd_per_call"].as_f64().unwrap_or(0.0)
            )),
            forecast["source"].as_str().unwrap_or("unavailable")
        ));
        lines.push(format!(
            "  runway      {} turn(s) · {} · {}",
            forecast["runway_turns"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            format_duration(forecast["runway_secs"].as_u64()),
            format_usd(forecast["runway_cost_usd"].as_f64())
        ));
    }
    if let Some(state) = status.get("state").filter(|value| value.is_object()) {
        lines.push(format!(
            "  auto        extensions={} · turns={} · no-progress={} · elapsed={}",
            state["extensions"].as_u64().unwrap_or(0),
            state["total_turns"].as_u64().unwrap_or(0),
            state["turns_without_progress"].as_u64().unwrap_or(0),
            format_duration(Some(state["elapsed_secs"].as_u64().unwrap_or(0)))
        ));
        if let Some(reason) = state["paused_reason"].as_str()
            && !reason.is_empty()
        {
            lines.push(format!(
                "  paused      {reason}{}",
                state["paused_at"]
                    .as_str()
                    .map(|at| format!(" at {at}"))
                    .unwrap_or_default()
            ));
        }
    } else {
        lines.push("  auto        not configured".to_string());
    }
    if let Some(policy) = status.get("policy").filter(|value| value.is_object())
        && mode == "auto"
    {
        lines.push(format!(
            "  policy      enabled={} · extend={:.2} · max×{:.2} · forecast={} call(s) · min runway={} turn(s)",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["extend_ratio"].as_f64().unwrap_or(0.0),
            policy["max_multiplier"].as_f64().unwrap_or(0.0),
            policy["forecast_calls"].as_u64().unwrap_or(0),
            policy["min_runway_turns"].as_u64().unwrap_or(0)
        ));
    }
    lines
}

fn forecast_lines(value: &Value, forecast: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "session {} budget forecast",
        value["session_id"].as_str().unwrap_or("?")
    )];
    lines.push(format!(
        "  samples     {} · {}",
        forecast["sample_calls"].as_u64().unwrap_or(0),
        forecast["source"].as_str().unwrap_or("unavailable")
    ));
    lines.push(format!(
        "  per call    {} input · {} output · {}",
        group_thousands(forecast["avg_input_tokens_per_call"].as_u64().unwrap_or(0)),
        group_thousands(forecast["avg_output_tokens_per_call"].as_u64().unwrap_or(0)),
        format_usd(Some(
            forecast["avg_cost_usd_per_call"].as_f64().unwrap_or(0.0)
        ))
    ));
    lines.push(format!(
        "  remaining   {} tokens · {} · {}",
        format_tokens(forecast["remaining_tokens"].as_u64()),
        format_usd(forecast["remaining_cost_usd"].as_f64()),
        format_duration(forecast["remaining_secs"].as_u64())
    ));
    lines.push(format!(
        "  runway      {} turn(s) · {} · {}",
        forecast["runway_turns"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        format_duration(forecast["runway_secs"].as_u64()),
        format_usd(forecast["runway_cost_usd"].as_f64())
    ));
    lines.push(format!(
        "  critical    {}",
        forecast["critical_path_turns"]
            .as_u64()
            .map(|turns| format!("{turns} turn(s)"))
            .unwrap_or_else(|| "unknown".to_string())
    ));
    lines.push(format!(
        "  generated   {}",
        format_optional_text(forecast.get("generated_at"))
    ));
    lines
}

fn project_run_policy_lines(value: &Value, policy: &Value) -> Vec<String> {
    let stop = policy
        .get("stop_policy")
        .cloned()
        .unwrap_or_else(|| json!({}));
    vec![
        format!(
            "project {} Run Contract defaults{}",
            value["project_id"].as_str().unwrap_or("?"),
            value["updated_at"]
                .as_str()
                .map(|at| format!(" · updated {at}"))
                .unwrap_or_else(|| " · built-in defaults".to_string())
        ),
        format!(
            "  profile     {}",
            policy["default_profile"].as_str().unwrap_or("balanced")
        ),
        format!(
            "  reserve     {:.0}% held for verification/merge",
            policy["completion_reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0
        ),
        format!(
            "  attention   cap {} weighted interrupt(s)",
            policy["attention_cap"].as_u64().unwrap_or(20)
        ),
        format!(
            "  auto rescue {} (one grant per session when the turn pauses only on completion_reserve_*)",
            if policy
                .get("auto_rescue")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!(
            "  auto envelopes {} (runtime task budget may reuse completed-task headroom; stored caps unchanged)",
            if policy
                .get("auto_reallocate_envelopes")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!(
            "  stop        no-progress {} turn(s) · evidence {} · partial delivery {}",
            stop["no_progress_turns"].as_u64().unwrap_or(6),
            if stop
                .get("require_evidence")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                "required"
            } else {
                "optional"
            },
            if stop
                .get("partial_delivery")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                "allowed"
            } else {
                "disabled"
            }
        ),
    ]
}

fn auto_policy_lines(value: &Value, policy: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "project {} auto budget{}",
        value["project_id"].as_str().unwrap_or("?"),
        value["updated_at"]
            .as_str()
            .map(|at| format!(" · updated {at}"))
            .unwrap_or_default()
    )];
    lines.push(format!(
        "  enabled     {}",
        policy["enabled"].as_bool().unwrap_or(false)
    ));
    lines.push(format!(
        "  initial     tokens {} · {} · {}",
        format_tokens(policy["initial_token_budget"].as_u64()),
        format_usd(policy["initial_cost_budget_usd"].as_f64()),
        format_duration(policy["initial_wall_clock_secs"].as_u64())
    ));
    lines.push(format!(
        "  hard caps   tokens {} · {} · {}",
        format_tokens(policy["hard_token_cap"].as_u64()),
        format_usd(policy["hard_cost_cap_usd"].as_f64()),
        format_duration(policy["hard_wall_clock_secs"].as_u64())
    ));
    lines.push(format!(
        "  extension   ratio {:.2} · max×{:.2} · forecast {} call(s) · min runway {} turn(s)",
        policy["extend_ratio"].as_f64().unwrap_or(0.0),
        policy["max_multiplier"].as_f64().unwrap_or(0.0),
        policy["forecast_calls"].as_u64().unwrap_or(0),
        policy["min_runway_turns"].as_u64().unwrap_or(0)
    ));
    lines.push(format!(
        "  warn/pause  {:.0}% / {:.0}%",
        policy["warn_at_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        policy["pause_at_ratio"].as_f64().unwrap_or(0.0) * 100.0
    ));
    lines.push(format!(
        "  turns       per run {} · total {} · no-progress {} ({})",
        policy["max_turns_per_run"].as_u64().unwrap_or(0),
        policy["max_total_turns"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        policy["no_progress_turns"].as_u64().unwrap_or(0),
        policy["no_progress_action"].as_str().unwrap_or("pause")
    ));
    lines.push(format!(
        "  tool bundle auto={}",
        policy["auto_tool_bundle"].as_bool().unwrap_or(false)
    ));
    lines
}

/// Pure line builder used by the printer and by CLI render tests.
pub(crate) fn budget_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(status) = status_object(value) {
        return Some(status_lines(status, value.get("budget").is_some()));
    }
    if let Some(forecast) = forecast_object(value) {
        return Some(forecast_lines(value, forecast));
    }
    if let Some(policy) = project_run_policy_object(value) {
        return Some(project_run_policy_lines(value, policy));
    }
    if let Some(policy) = auto_policy_object(value) {
        return Some(auto_policy_lines(value, policy));
    }
    None
}

pub(crate) fn print(value: &Value) -> bool {
    let Some(lines) = budget_lines(value) else {
        return false;
    };
    for line in lines {
        println!("{line}");
    }
    true
}

fn status_csv_row(status: &Value) -> Vec<String> {
    let forecast = status.get("forecast").cloned().unwrap_or_else(|| json!({}));
    let state = status.get("state").cloned().unwrap_or_else(|| json!({}));
    vec![
        status["session_id"].as_str().unwrap_or("").to_string(),
        status["mode"].as_str().unwrap_or("").to_string(),
        status["unlimited"].as_bool().unwrap_or(false).to_string(),
        status["effective_token_budget"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["effective_cost_budget_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["effective_time_budget_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["hard_token_cap"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["hard_cost_cap_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["hard_wall_clock_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["spent_tokens"].as_u64().unwrap_or(0).to_string(),
        status["spent_cost_usd"].as_f64().unwrap_or(0.0).to_string(),
        status["elapsed_secs"].as_u64().unwrap_or(0).to_string(),
        status["remaining_tokens"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["remaining_cost_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        status["remaining_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["sample_calls"].as_u64().unwrap_or(0).to_string(),
        forecast["runway_turns"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["source"].as_str().unwrap_or("").to_string(),
        state["extensions"].as_u64().unwrap_or(0).to_string(),
        state["turns_without_progress"]
            .as_u64()
            .unwrap_or(0)
            .to_string(),
        state["paused_reason"].as_str().unwrap_or("").to_string(),
    ]
}

const STATUS_COLUMNS: &[&str] = &[
    "session_id",
    "mode",
    "unlimited",
    "effective_token_budget",
    "effective_cost_budget_usd",
    "effective_time_budget_secs",
    "hard_token_cap",
    "hard_cost_cap_usd",
    "hard_wall_clock_secs",
    "spent_tokens",
    "spent_cost_usd",
    "elapsed_secs",
    "remaining_tokens",
    "remaining_cost_usd",
    "remaining_secs",
    "forecast_sample_calls",
    "forecast_runway_turns",
    "forecast_source",
    "auto_extensions",
    "auto_turns_without_progress",
    "auto_paused_reason",
];

pub(crate) fn session_budget_csv(value: &Value) -> String {
    let status = status_object(value).unwrap_or(value);
    let mut output = STATUS_COLUMNS.join(",");
    output.push('\n');
    output.push_str(
        &status_csv_row(status)
            .iter()
            .map(|cell| csv_cell(cell))
            .collect::<Vec<_>>()
            .join(","),
    );
    output.push('\n');
    output
}

pub(crate) fn session_budget_forecast_csv(value: &Value) -> String {
    const COLUMNS: &[&str] = &[
        "session_id",
        "sample_calls",
        "avg_input_tokens_per_call",
        "avg_output_tokens_per_call",
        "avg_cost_usd_per_call",
        "remaining_tokens",
        "remaining_cost_usd",
        "remaining_secs",
        "runway_turns",
        "runway_secs",
        "runway_cost_usd",
        "critical_path_turns",
        "source",
        "generated_at",
    ];
    let forecast = value.get("forecast").unwrap_or(value);
    let row = [
        value["session_id"].as_str().unwrap_or("").to_string(),
        forecast["sample_calls"].as_u64().unwrap_or(0).to_string(),
        forecast["avg_input_tokens_per_call"]
            .as_u64()
            .unwrap_or(0)
            .to_string(),
        forecast["avg_output_tokens_per_call"]
            .as_u64()
            .unwrap_or(0)
            .to_string(),
        forecast["avg_cost_usd_per_call"]
            .as_f64()
            .unwrap_or(0.0)
            .to_string(),
        forecast["remaining_tokens"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["remaining_cost_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["remaining_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["runway_turns"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["runway_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["runway_cost_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["critical_path_turns"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        forecast["source"].as_str().unwrap_or("").to_string(),
        forecast["generated_at"].as_str().unwrap_or("").to_string(),
    ];
    let mut output = COLUMNS.join(",");
    output.push('\n');
    output.push_str(
        &row.iter()
            .map(|cell| csv_cell(cell))
            .collect::<Vec<_>>()
            .join(","),
    );
    output.push('\n');
    output
}

pub(crate) fn project_auto_budget_csv(value: &Value) -> String {
    const COLUMNS: &[&str] = &[
        "project_id",
        "updated_at",
        "enabled",
        "initial_token_budget",
        "initial_cost_budget_usd",
        "initial_wall_clock_secs",
        "hard_token_cap",
        "hard_cost_cap_usd",
        "hard_wall_clock_secs",
        "extend_ratio",
        "max_multiplier",
        "forecast_calls",
        "min_runway_turns",
        "warn_at_ratio",
        "pause_at_ratio",
        "max_turns_per_run",
        "max_total_turns",
        "no_progress_turns",
        "no_progress_action",
        "auto_tool_bundle",
    ];
    let policy = auto_policy_object(value).unwrap_or(value);
    let row = [
        value["project_id"].as_str().unwrap_or("").to_string(),
        value["updated_at"].as_str().unwrap_or("").to_string(),
        policy["enabled"].as_bool().unwrap_or(false).to_string(),
        policy["initial_token_budget"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["initial_cost_budget_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["initial_wall_clock_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["hard_token_cap"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["hard_cost_cap_usd"]
            .as_f64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["hard_wall_clock_secs"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["extend_ratio"].as_f64().unwrap_or(0.0).to_string(),
        policy["max_multiplier"].as_f64().unwrap_or(0.0).to_string(),
        policy["forecast_calls"].as_u64().unwrap_or(0).to_string(),
        policy["min_runway_turns"].as_u64().unwrap_or(0).to_string(),
        policy["warn_at_ratio"].as_f64().unwrap_or(0.0).to_string(),
        policy["pause_at_ratio"].as_f64().unwrap_or(0.0).to_string(),
        policy["max_turns_per_run"]
            .as_u64()
            .unwrap_or(0)
            .to_string(),
        policy["max_total_turns"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_default(),
        policy["no_progress_turns"]
            .as_u64()
            .unwrap_or(0)
            .to_string(),
        policy["no_progress_action"]
            .as_str()
            .unwrap_or("pause")
            .to_string(),
        policy["auto_tool_bundle"]
            .as_bool()
            .unwrap_or(false)
            .to_string(),
    ];
    let mut output = COLUMNS.join(",");
    output.push('\n');
    output.push_str(
        &row.iter()
            .map(|cell| csv_cell(cell))
            .collect::<Vec<_>>()
            .join(","),
    );
    output.push('\n');
    output
}

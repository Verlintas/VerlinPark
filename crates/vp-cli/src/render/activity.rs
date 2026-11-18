//! Human renderers for activity, checkpoints, governors, and consoles.

use super::*;

pub(crate) fn tool_activity_lines(value: &Value) -> Option<Vec<String>> {
    let entries = value.get("entries")?.as_array()?;
    let summary = value.get("summary")?;
    let calls = summary.get("calls")?.as_u64()?;
    value.get("total").and_then(Value::as_u64)?;
    if !entries
        .iter()
        .all(|entry| entry.get("duration_ms").is_some())
    {
        return None;
    }
    let mut lines = vec![format!(
        "{} tool call(s) · {} error(s) · {} retryable · avg {} ms · max {} ms · {} output bytes{}",
        calls,
        summary["errors"].as_u64().unwrap_or(0),
        summary["retryable_errors"].as_u64().unwrap_or(0),
        summary["average_duration_ms"].as_u64().unwrap_or(0),
        summary["max_duration_ms"].as_u64().unwrap_or(0),
        summary["output_bytes"].as_u64().unwrap_or(0),
        if value["truncated"].as_bool().unwrap_or(false) {
            " · older events omitted"
        } else {
            ""
        }
    )];
    if entries.is_empty() {
        lines.push("No tool activity recorded for this session.".to_string());
        return Some(lines);
    }
    for entry in entries {
        let status = if entry["is_error"].as_bool().unwrap_or(false) {
            entry["error_code"].as_str().unwrap_or("ERROR")
        } else {
            "ok"
        };
        let retry = if entry["is_error"].as_bool().unwrap_or(false) {
            if entry["retryable"].as_bool().unwrap_or(false) {
                " · retryable"
            } else {
                ""
            }
        } else {
            ""
        };
        let truncated = if entry["truncated"].as_bool().unwrap_or(false) {
            " · truncated"
        } else {
            ""
        };
        lines.push(format!(
            "{}  {:<18} {:<9} {:>6} ms  {:>8} bytes{}{}",
            entry["created_at"].as_str().unwrap_or("?"),
            entry["name"].as_str().unwrap_or("unknown"),
            status,
            entry["duration_ms"].as_u64().unwrap_or(0),
            entry["output_bytes"].as_u64().unwrap_or(0),
            truncated,
            retry
        ));
        if let Some(hint) = entry["hint"].as_str().filter(|hint| !hint.is_empty()) {
            lines.push(format!("    hint: {hint}"));
        }
    }
    Some(lines)
}

pub(crate) fn context_checkpoint_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(checkpoints) = value.get("checkpoints").and_then(Value::as_array)
        && value.get("latest_id").is_some()
    {
        let mut lines = Vec::new();
        lines.push(format!(
            "context checkpoints for {} ({} shown, latest {})",
            value["session_id"].as_str().unwrap_or("?"),
            checkpoints.len(),
            value["latest_id"].as_str().unwrap_or("none")
        ));
        let policy = &value["policy"];
        lines.push(format!(
            "retention: sweep {} · max {} · age {}d",
            if policy["enabled"].as_bool().unwrap_or(false) {
                "on"
            } else {
                "off"
            },
            policy["max_checkpoints"].as_u64().unwrap_or(20),
            policy["max_age_days"].as_u64().unwrap_or(30)
        ));
        for checkpoint in checkpoints {
            let before = checkpoint["tokens_before"].as_u64().unwrap_or(0);
            let after = checkpoint["tokens_after"].as_u64().unwrap_or(0);
            let delta = before.saturating_sub(after);
            let summary = checkpoint["summary"]
                .as_str()
                .or_else(|| checkpoint["label"].as_str())
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let preview = if summary.chars().count() > 80 {
                format!("{}…", summary.chars().take(80).collect::<String>())
            } else {
                summary
            };
            lines.push(format!(
                "{}  {:<8} rev {:<3} {:>7}→{:<7} −{:<7} {}  {}",
                checkpoint["id"].as_str().unwrap_or("?"),
                checkpoint["mode"].as_str().unwrap_or("?"),
                checkpoint["plan_revision"].as_u64().unwrap_or(0),
                before,
                after,
                delta,
                checkpoint["created_at"].as_str().unwrap_or(""),
                preview
            ));
        }
        return Some(lines);
    }
    if let Some(checkpoint) = value.get("checkpoint").filter(|checkpoint| {
        checkpoint.get("mode").is_some() && checkpoint.get("tokens_after").is_some()
    }) && let Some(preflight) = value.get("preflight")
    {
        let mut lines = vec![format!(
            "{}  {}  {}  rev {}  {} → {} tokens",
            checkpoint["id"].as_str().unwrap_or("?"),
            checkpoint["mode"].as_str().unwrap_or("?"),
            checkpoint["label"].as_str().unwrap_or(""),
            checkpoint["plan_revision"].as_u64().unwrap_or(0),
            checkpoint["tokens_before"].as_u64().unwrap_or(0),
            checkpoint["tokens_after"].as_u64().unwrap_or(0)
        )];
        lines.push(format!(
            "active boundary: {} · messages {} · estimate {} / hard {}",
            if value["is_active"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            },
            preflight["active_messages"].as_u64().unwrap_or(0),
            preflight["estimated_tokens"].as_u64().unwrap_or(0),
            preflight["hard_limit_tokens"].as_u64().unwrap_or(0)
        ));
        for error in preflight["errors"].as_array().into_iter().flatten() {
            if let Some(error) = error.as_str() {
                lines.push(format!("error: {error}"));
            }
        }
        for warning in preflight["warnings"].as_array().into_iter().flatten() {
            if let Some(warning) = warning.as_str() {
                lines.push(format!("warning: {warning}"));
            }
        }
        return Some(lines);
    }
    if let Some(restored) = value.get("restored_event_seq") {
        return Some(vec![format!(
            "restored {} ({}) · {} active messages · {} tokens · events {}/{}",
            value["checkpoint_id"].as_str().unwrap_or("?"),
            value["mode"].as_str().unwrap_or("?"),
            value["active_messages"].as_u64().unwrap_or(0),
            value["estimated_tokens"].as_u64().unwrap_or(0),
            value["compaction_event_seq"].as_u64().unwrap_or(0),
            restored.as_u64().unwrap_or(0)
        )]);
    }
    if value.get("removed").is_some()
        && let Some(checkpoint_id) = value.get("checkpoint_id").and_then(Value::as_str)
        && value.get("session_id").is_some()
    {
        return Some(vec![format!(
            "{} {}",
            checkpoint_id,
            if value["removed"].as_bool().unwrap_or(false) {
                "deleted"
            } else {
                "not found"
            }
        )]);
    }
    if let Some(candidates) = value.get("candidates").and_then(Value::as_u64)
        && value.get("session_id").is_some()
        && value
            .get("checkpoints")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row.get("tokens_before").is_some()))
    {
        return Some(vec![format!(
            "{}pruned {} of {} candidate context checkpoint(s)",
            if value["dry_run"].as_bool().unwrap_or(false) {
                "dry run: "
            } else {
                ""
            },
            value["removed"].as_u64().unwrap_or(0),
            candidates
        )]);
    }
    if let Some(project_id) = value.get("project_id").and_then(Value::as_str)
        && let Some(policy) = value
            .get("policy")
            .filter(|policy| policy.get("max_checkpoints").is_some())
    {
        return Some(vec![format!(
            "{}: context checkpoint sweep {} · max {} · age {}d",
            project_id,
            if policy["enabled"].as_bool().unwrap_or(false) {
                "on"
            } else {
                "off"
            },
            policy["max_checkpoints"].as_u64().unwrap_or(20),
            policy["max_age_days"].as_u64().unwrap_or(30)
        )]);
    }
    None
}

pub(crate) fn governor_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(policy) = value.get("policy")
        && value.get("session_id").is_none()
        && policy.get("reserve_ratio").is_some()
    {
        let enabled = policy["enabled"].as_bool().unwrap_or(false);
        let ratio = policy["reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0;
        let throttle = policy["throttle_at_ratio"].as_f64().unwrap_or(0.0) * 100.0;
        let mode = policy["mode"].as_str().unwrap_or("single");
        let mut lines = vec![format!(
            "Critical-path governor: {} · {} · reserve {:.0}% · throttle {:.0}% · boost {} · min {} tokens",
            if enabled { "enabled" } else { "disabled" },
            mode,
            ratio.max(0.0),
            throttle.max(0.0),
            policy["critical_boost"].as_i64().unwrap_or(0),
            policy["min_task_budget"].as_u64().unwrap_or(0)
        )];
        if mode == "multi" {
            lines.push(format!(
                "Fairness: max {} paths · department floor {:.0}% · observed {:.0}% · starvation {} s",
                policy["max_paths"].as_u64().unwrap_or(4),
                policy["department_min_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                policy["observed_weight_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                policy["starvation_after_secs"].as_u64().unwrap_or(0)
            ));
        }
        return Some(lines);
    }
    let session = value.get("session_id")?.as_str()?;
    let reserve = value.get("reserve")?;
    let mut lines = Vec::new();
    lines.push(format!(
        "Governor {}{} · session {session} · plan r{} · {}{}",
        if value["enabled"].as_bool().unwrap_or(false) {
            "enabled"
        } else {
            "disabled"
        },
        reserve["state"]
            .as_str()
            .map(|state| format!(" ({state})"))
            .unwrap_or_default(),
        value["plan_revision"].as_u64().unwrap_or(0),
        value["session_status"].as_str().unwrap_or("unknown"),
        if value["unlimited"].as_bool().unwrap_or(false) {
            " · unlimited"
        } else {
            ""
        }
    ));
    if let Some(path) = value
        .get("critical_path")
        .filter(|path| path["task_ids"].is_array())
    {
        let tasks = path["task_ids"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" -> ")
            })
            .unwrap_or_default();
        if tasks.is_empty() {
            lines.push(format!(
                "Path: complete · weight {} tokens",
                path["weight_tokens"].as_u64().unwrap_or(0)
            ));
        } else {
            lines.push(format!(
                "Path (weight {} tokens): {tasks}",
                path["weight_tokens"].as_u64().unwrap_or(0)
            ));
        }
    }
    lines.push(format!(
        "Reserve: protected {} of remaining {} · ratio {:.0}% · throttle {:.0}%{}",
        reserve["protected_tokens"].as_u64().unwrap_or(0),
        reserve["remaining_tokens"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unbounded".to_string()),
        reserve["reserve_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        reserve["throttle_at_ratio"].as_f64().unwrap_or(0.0) * 100.0,
        if reserve["throttled"].as_bool().unwrap_or(false) {
            " · throttled"
        } else {
            ""
        }
    ));
    let mode = reserve["mode"]
        .as_str()
        .or_else(|| value["policy"]["mode"].as_str())
        .unwrap_or("single");
    if mode == "multi" {
        lines.push(format!(
            "Fairness: {} path(s) · allocated {} · floors {} · observed {}%{}{}",
            reserve["path_count"].as_u64().unwrap_or(0),
            reserve["allocated_tokens"].as_u64().unwrap_or(0),
            reserve["floor_tokens"].as_u64().unwrap_or(0),
            value["policy"]["observed_weight_ratio"]
                .as_f64()
                .unwrap_or(0.0)
                * 100.0,
            reserve["enumerated_paths"]
                .as_u64()
                .map(|value| format!(" · {value} enumerated"))
                .unwrap_or_default(),
            if reserve["paths_truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        ));
        if let Some(paths) = value["paths"].as_array().filter(|paths| !paths.is_empty()) {
            lines.push("Path table:".to_string());
            for path in paths.iter().take(4) {
                let tasks = path["task_ids"]
                    .as_array()
                    .map(|tasks| {
                        tasks
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" -> ")
                    })
                    .unwrap_or_default();
                let departments = path["departments"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  #{} weight {} · floor {} · alloc {} · deficit {} · [{}] {}",
                    path["path_id"].as_u64().unwrap_or(0),
                    path["weight_tokens"].as_u64().unwrap_or(0),
                    path["floor_tokens"].as_u64().unwrap_or(0),
                    path["allocation_tokens"].as_u64().unwrap_or(0),
                    path["deficit_tokens"].as_u64().unwrap_or(0),
                    departments,
                    tasks
                ));
            }
            if paths.len() > 4 {
                lines.push(format!("  +{} more protected paths", paths.len() - 4));
            }
        }
        if let Some(departments) = value["departments"]
            .as_array()
            .filter(|departments| !departments.is_empty())
        {
            lines.push("Department floors:".to_string());
            for department in departments.iter().take(6) {
                let path_ids = department["path_ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_u64)
                            .map(|id| format!("#{id}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "—".to_string());
                lines.push(format!(
                    "  {} weight {} · floor {} · paths {}",
                    department["department"].as_str().unwrap_or("—"),
                    department["weight_tokens"].as_u64().unwrap_or(0),
                    department["floor_tokens"].as_u64().unwrap_or(0),
                    path_ids
                ));
            }
            if departments.len() > 6 {
                lines.push(format!("  +{} more departments", departments.len() - 6));
            }
        }
    }
    if let Some(admitted) = value["admitted"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        lines.push(format!(
            "Admit: {}",
            admitted
                .iter()
                .filter_map(Value::as_str)
                .take(8)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(deferred) = value["deferred"]
        .as_array()
        .filter(|items| !items.is_empty())
    {
        for deferral in deferred.iter().take(5) {
            lines.push(format!(
                "Defer: {} ({}, {})",
                deferral["task_id"].as_str().unwrap_or("?"),
                deferral["reason"].as_str().unwrap_or("?"),
                deferral["code"].as_str().unwrap_or("-")
            ));
        }
        if deferred.len() > 5 {
            lines.push(format!("  +{} more deferred tasks", deferred.len() - 5));
        }
        if let Some(order) = value["deferred_order"]
            .as_array()
            .filter(|order| !order.is_empty())
        {
            lines.push(format!(
                "Deferred order: {}",
                order
                    .iter()
                    .filter_map(Value::as_str)
                    .take(8)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    if let Some(error) = value.get("error").filter(|error| error["code"].is_string()) {
        lines.push(format!(
            "Error: {} · {}",
            error["code"].as_str().unwrap_or("?"),
            error["message"].as_str().unwrap_or("")
        ));
        if let Some(hint) = error["hint"].as_str() {
            lines.push(format!("  hint: {hint}"));
        }
    }
    if let Some(last) = value["last_decisions"]
        .as_array()
        .and_then(|decisions| decisions.last())
    {
        lines.push(format!(
            "Last decision: {} {} at {} · {}",
            last["action"].as_str().unwrap_or("?"),
            last["reason"].as_str().unwrap_or("?"),
            last["created_at"].as_str().unwrap_or("?"),
            last["detail"].as_str().unwrap_or("")
        ));
    }
    Some(lines)
}

pub(crate) fn process_console_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_processes").is_some())
    {
        let project = value["project_id"].as_str().unwrap_or("?");
        return Some(vec![
            format!("project {project} process policy"),
            format!(
                "  idle sweep {} · max {} · idle TTL {}s",
                if policy["idle_sweep_enabled"].as_bool().unwrap_or(false) {
                    "on"
                } else {
                    "off"
                },
                policy["max_processes"].as_u64().unwrap_or(0),
                policy["idle_ttl_secs"].as_u64().unwrap_or(0),
            ),
            format!(
                "  retained log {} bytes / {} lines",
                policy["max_output_bytes"].as_u64().unwrap_or(0),
                policy["max_output_lines"].as_u64().unwrap_or(0),
            ),
        ]);
    }
    if let Some(processes) = value.get("processes").and_then(Value::as_array)
        && value.get("active").is_some()
    {
        if processes.is_empty() {
            return Some(vec!["No supervised processes.".to_string()]);
        }
        let mut lines = Vec::new();
        for process in processes {
            let id = process["id"].as_str().unwrap_or("?");
            let state = process["state"].as_str().unwrap_or("?");
            let name = process["name"].as_str().unwrap_or("");
            let pid = process["pid"].as_i64().unwrap_or(0);
            let elapsed = process["started_at"].as_str().unwrap_or("");
            let exit = process["exit_code"]
                .as_i64()
                .map(|code| format!("  exit {code}"))
                .unwrap_or_default();
            lines.push(format!(
                "{id}  {state:<8}  pid {pid:<7}  {name}{exit}  started {elapsed}  {} bytes",
                process["output_bytes"].as_u64().unwrap_or(0)
            ));
            if let Some(detail) = process["detail"].as_str() {
                lines.push(format!("  {detail}"));
            }
        }
        return Some(lines);
    }
    if let Some(process_id) = value.get("process_id").and_then(Value::as_str)
        && let Some(content) = value.get("content").and_then(Value::as_str)
        && value.get("next_offset").is_some()
    {
        let mut lines = vec![format!(
            "{process_id}  {}  bytes {}/{}  offset {} -> {}{}",
            value["state"].as_str().unwrap_or("?"),
            value["content"].as_str().map(str::len).unwrap_or(0),
            value["total_bytes"].as_u64().unwrap_or(0),
            value["offset"].as_u64().unwrap_or(0),
            value["next_offset"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                "  (truncated)"
            } else {
                ""
            }
        )];
        if !content.is_empty() {
            lines.push(content.to_string());
        }
        return Some(lines);
    }
    if let Some(process) = value.get("process")
        && value.get("forced").is_some()
    {
        return Some(vec![format!(
            "{}  {}  exit {}{}  waited {}ms{}",
            process["id"].as_str().unwrap_or("?"),
            process["state"].as_str().unwrap_or("?"),
            process["exit_code"].as_i64().unwrap_or(-1),
            process["signal"]
                .as_i64()
                .map(|signal| format!("  signal {signal}"))
                .unwrap_or_default(),
            value["waited_ms"].as_u64().unwrap_or(0),
            if value["forced"].as_bool().unwrap_or(false) {
                "  (forced)"
            } else {
                ""
            }
        )]);
    }
    None
}

pub(crate) fn test_cache_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(map) = value.get("map") {
        let mut lines = vec![format!(
            "test impact  session {}  project {}  {} target(s)  generation {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            map["targets"].as_array().map(Vec::len).unwrap_or(0),
            map["generation"].as_str().unwrap_or("?")
        )];
        if let Some(command) = value.get("test_command").and_then(Value::as_str) {
            lines.push(format!("  command: {command}"));
        }
        lines.push(format!(
            "  lsp_available={} lexical_only={} truncated={} changed={}",
            map["lsp_available"].as_bool().unwrap_or(false),
            map["lexical_only"].as_bool().unwrap_or(false),
            map["truncated"].as_bool().unwrap_or(false),
            map["changed_paths"].as_array().map(Vec::len).unwrap_or(0)
        ));
        for target in map["targets"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  {:<42} {:<8} {:>5.0}%  {}",
                target["path"].as_str().unwrap_or("?"),
                target["source"].as_str().unwrap_or("?"),
                target["confidence"].as_f64().unwrap_or(0.0) * 100.0,
                target["reason"].as_str().unwrap_or("")
            ));
        }
        for note in map["notes"].as_array().into_iter().flatten() {
            lines.push(format!("  note: {}", note.as_str().unwrap_or("?")));
        }
        return Some(lines);
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && value.get("total").is_some()
        && value.get("project_id").is_some()
    {
        let mut lines = vec![format!(
            "test cache entries  {}  {} of {} shown",
            value["project_id"].as_str().unwrap_or("?"),
            entries.len(),
            value["total"].as_u64().unwrap_or(0)
        )];
        for entry in entries {
            lines.push(format!(
                "  {}  {:<5} {:<6}  exit {:<3}  {:>7}ms  hits {:<4}  {}",
                entry["id"].as_str().unwrap_or("?"),
                entry["kind"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["exit_code"].as_i64().unwrap_or(0),
                entry["duration_ms"].as_u64().unwrap_or(0),
                entry["hit_count"].as_u64().unwrap_or(0),
                entry["command"].as_str().unwrap_or("")
            ));
        }
        if entries.is_empty() {
            lines.push("  no cached results".to_string());
        }
        return Some(lines);
    }
    if value.get("pruned").is_some() && value.get("affected_sessions").is_some() {
        let mut lines = vec![format!(
            "test cache prune  {}  {} removed · {} remaining · {} session(s) · dry_run={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["pruned"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0),
            value["affected_sessions"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        )];
        if let Some(ids) = value.get("removed_ids").and_then(Value::as_array) {
            for id in ids.iter().take(20).filter_map(Value::as_str) {
                lines.push(format!("  removed {id}"));
            }
        }
        return Some(lines);
    }
    if value.get("cleared").is_some() && value.get("affected_sessions").is_some() {
        return Some(vec![format!(
            "test cache clear  {}  {} cleared · {} remaining · {} session(s)",
            value["project_id"].as_str().unwrap_or("?"),
            value["cleared"].as_u64().unwrap_or(0),
            value["remaining"].as_u64().unwrap_or(0),
            value["affected_sessions"].as_u64().unwrap_or(0)
        )]);
    }
    if let Some(policy) = value
        .get("policy")
        .filter(|policy| policy.get("max_output_bytes").is_some())
    {
        let mut lines = vec![format!(
            "test cache policy  {}  enabled={}  max-entries={}  retention={}d  output≤{}B  reuse-lint={}  integrator={}  failed-ttl={}s",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(true),
            policy["max_entries"].as_u64().unwrap_or(0),
            policy["retention_days"].as_u64().unwrap_or(0),
            policy["max_output_bytes"].as_u64().unwrap_or(0),
            policy["reuse_lint"].as_bool().unwrap_or(true),
            policy["include_integration"].as_bool().unwrap_or(false),
            policy["failed_retention_secs"].as_u64().unwrap_or(0)
        )];
        if value.get("entries").is_some_and(Value::is_u64) {
            lines.push(format!(
                "  entries={} passed={} failed={} error={} expired={} hits={} output={}B",
                value["entries"].as_u64().unwrap_or(0),
                value["passed"].as_u64().unwrap_or(0),
                value["failed"].as_u64().unwrap_or(0),
                value["error"].as_u64().unwrap_or(0),
                value["expired"].as_u64().unwrap_or(0),
                value["total_hits"].as_u64().unwrap_or(0),
                value["output_bytes"].as_u64().unwrap_or(0)
            ));
            if let Some(kinds) = value.get("by_kind").and_then(Value::as_object) {
                let summary = kinds
                    .iter()
                    .map(|(kind, count)| format!("{kind}={}", count.as_u64().unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !summary.is_empty() {
                    lines.push(format!("  kinds: {summary}"));
                }
            }
        }
        return Some(lines);
    }
    None
}

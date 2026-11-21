//! Human renderer for the V1 Collab Agent Inspector and direct channel.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(lines) = agent_inspect_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = agent_message_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    false
}

pub(crate) fn agent_inspect_lines(value: &Value) -> Option<Vec<String>> {
    let agent = value.get("agent")?;
    let context = value.get("context")?;
    let timeline = value.get("timeline")?.as_array()?;
    if agent.get("session_id").is_none() || agent.get("role").is_none() {
        return None;
    }

    let mut lines = Vec::new();
    let mut header = vec![
        agent["name"].as_str().unwrap_or("?").to_string(),
        agent["role"].as_str().unwrap_or("?").to_string(),
        agent["status"].as_str().unwrap_or("?").to_string(),
    ];
    if let Some(title) = agent["title"].as_str() {
        header.push(title.to_string());
    }
    lines.push(header.join(" · "));
    lines.push(format!(
        "id {} · session {} · model {}",
        agent["id"].as_str().unwrap_or("?"),
        agent["session_id"].as_str().unwrap_or("?"),
        agent["model"].as_str().unwrap_or("?")
    ));
    let mut placement = Vec::new();
    if let Some(department) = agent["department"].as_str() {
        placement.push(format!("department {department}"));
    }
    if let Some(profile) = agent["profile"].as_str() {
        placement.push(format!("profile {profile}"));
    }
    if let Some(parent) = agent["parent_id"].as_str() {
        placement.push(format!("parent {parent}"));
    }
    if let Some(identity) = agent["identity_id"].as_str() {
        placement.push(format!("identity {identity}"));
    }
    if !placement.is_empty() {
        lines.push(placement.join(" · "));
    }
    let mut workspace = Vec::new();
    if let Some(worktree) = agent["worktree"].as_str() {
        workspace.push(format!("worktree {worktree}"));
    }
    if let Some(branch) = agent["branch"].as_str() {
        workspace.push(format!("branch {branch}"));
    }
    if let Some(workspace_id) = agent["workspace_id"].as_str() {
        workspace.push(format!("workspace {workspace_id}"));
    }
    if !workspace.is_empty() {
        lines.push(workspace.join(" · "));
    }

    if let Some(task) = value.get("task").filter(|task| !task.is_null()) {
        let mut row = vec![format!(
            "task {} · {}",
            task["task_id"].as_str().unwrap_or("?"),
            task["title"].as_str().unwrap_or("")
        )];
        if let Some(status) = task["status"].as_str() {
            row.push(format!("status {status}"));
        }
        if let Some(department) = task["department"].as_str() {
            row.push(format!("department {department}"));
        }
        if let Some(kind) = task["kind"].as_str() {
            row.push(format!("kind {kind}"));
        }
        row.push(format!(
            "attempts {}/{}",
            task["attempts"].as_u64().unwrap_or(0),
            task["max_attempts"].as_u64().unwrap_or(0)
        ));
        if let Some(verifier) = task["verifier_status"].as_str() {
            row.push(format!("verifier {verifier}"));
        }
        lines.push(row.join(" · "));
        if let Some(acceptance) = task["acceptance"].as_array()
            && !acceptance.is_empty()
        {
            lines.push(format!(
                "  acceptance: {}",
                acceptance
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
        if let Some(scope) = task["write_scope"].as_array()
            && !scope.is_empty()
        {
            lines.push(format!(
                "  write scope: {}",
                scope
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    } else {
        lines.push("task: none (session-level run)".to_string());
    }

    if let Some(objective) = context["objective"].as_str() {
        lines.push(format!("objective: {objective}"));
    }
    let mut context_row = vec![format!(
        "context {}/{} tokens",
        context["estimated_tokens"].as_u64().unwrap_or(0),
        context["context_window"].as_u64().unwrap_or(0)
    )];
    if let Some(threshold) = context["threshold_tokens"].as_u64() {
        context_row.push(format!("threshold {threshold}"));
    }
    if let Some(hard) = context["hard_limit_tokens"].as_u64() {
        context_row.push(format!("hard {hard}"));
    }
    lines.push(context_row.join(" · "));
    lines.push(format!(
        "usage in {} · out {} · cache read {} · cache write {} · cost ${:.4}",
        context["input_tokens"].as_u64().unwrap_or(0),
        context["output_tokens"].as_u64().unwrap_or(0),
        context["cache_read_tokens"].as_u64().unwrap_or(0),
        context["cache_write_tokens"].as_u64().unwrap_or(0),
        context["cost_usd"].as_f64().unwrap_or(0.0)
    ));
    let mut runtime = Vec::new();
    if let Some(bundle) = context["tool_bundle"].as_str() {
        runtime.push(format!(
            "tools {bundle}/{}",
            context["tool_count"].as_u64().unwrap_or(0)
        ));
    }
    if let Some(prefix) = context["prefix_hash"].as_str() {
        runtime.push(format!("prefix {}", short_hash(prefix)));
    }
    if let Some(compaction) = context["last_compaction"].as_str() {
        runtime.push(format!("last compaction {compaction}"));
    }
    if let (Some(chars), Some(hash)) = (
        context["memory_brief_chars"].as_u64(),
        context["memory_brief_hash"].as_str(),
    ) {
        runtime.push(format!("memory brief {chars} chars/{}", short_hash(hash)));
    }
    if !runtime.is_empty() {
        lines.push(runtime.join(" · "));
    }

    let steering = value["steering"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let inbox = value["inbox"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let pending = steering
        .iter()
        .filter(|row| row["status"].as_str() == Some("pending"))
        .count();
    lines.push(format!(
        "timeline {} row(s){} · steering {} ({pending} pending) · inbox {} · unread {} / read {} / done {}",
        timeline.len(),
        if value["truncated"].as_bool().unwrap_or(false) {
            " [truncated]"
        } else {
            ""
        },
        steering.len(),
        inbox.len(),
        value["inbox_counts"]["unread"].as_u64().unwrap_or(0),
        value["inbox_counts"]["read"].as_u64().unwrap_or(0),
        value["inbox_counts"]["done"].as_u64().unwrap_or(0)
    ));

    if !timeline.is_empty() {
        lines.push("Recent activity (oldest first)".to_string());
        let start = timeline.len().saturating_sub(10);
        for entry in &timeline[start..] {
            let mut row = vec![format!(
                "#{} {:<12} {:<11} {}",
                entry["seq"].as_u64().unwrap_or(0),
                entry["category"].as_str().unwrap_or("?"),
                entry["kind"].as_str().unwrap_or("?"),
                entry["created_at"].as_str().unwrap_or("?")
            )];
            if let Some(status) = entry["status"].as_str() {
                row.push(format!("status {status}"));
            }
            if entry["is_error"].as_bool().unwrap_or(false) {
                row.push("error".to_string());
            }
            if let Some(tool) = entry["tool"].as_str() {
                row.push(format!("tool {tool}"));
            }
            lines.push(format!("  {}", row.join(" · ")));
            let text = entry["text"].as_str().unwrap_or("").trim();
            if !text.is_empty() {
                lines.push(format!("    {text}"));
            }
        }
    }
    if !steering.is_empty() {
        lines.push("Steering".to_string());
        for row in steering.iter().rev().take(5).rev() {
            lines.push(format!(
                "  {} · {} · task {} · {}",
                row["id"].as_str().unwrap_or("?"),
                row["status"].as_str().unwrap_or("?"),
                row["task_id"].as_str().unwrap_or("-"),
                row["created_at"].as_str().unwrap_or("?")
            ));
            if let Some(body) = row["body"].as_str() {
                lines.push(format!("    {body}"));
            }
        }
    }
    if !inbox.is_empty() {
        lines.push("Inbox".to_string());
        for row in inbox.iter().take(5) {
            lines.push(format!(
                "  {} · {} · from {} · {}",
                row["id"].as_str().unwrap_or("?"),
                row["status"].as_str().unwrap_or("?"),
                row["from_label"].as_str().unwrap_or("?"),
                row["created_at"].as_str().unwrap_or("?")
            ));
            if let Some(body) = row["body"].as_str()
                && !body.is_empty()
            {
                lines.push(format!("    {body}"));
            }
        }
    }
    Some(lines)
}

pub(crate) fn agent_message_lines(value: &Value) -> Option<Vec<String>> {
    let delivery = value.get("delivery")?;
    let kind = delivery.get("kind")?.as_str()?;
    let status = delivery["status"].as_str().unwrap_or("?");
    let mut lines = vec![format!(
        "{kind} · {status} · agent {}",
        delivery["agent_id"].as_str().unwrap_or("?")
    )];
    if let Some(identity) = delivery["identity_id"].as_str() {
        lines.push(format!("identity {identity}"));
    }
    let message = delivery["message"].as_str().unwrap_or("");
    if !message.is_empty() {
        lines.push(message.to_string());
    }
    if let Some(steering) = delivery.get("steering").filter(|row| !row.is_null()) {
        lines.push(format!(
            "steering {} · task {} · created {}{}",
            steering["id"].as_str().unwrap_or("?"),
            steering["task_id"].as_str().unwrap_or("-"),
            steering["created_at"].as_str().unwrap_or("?"),
            steering["delivered_at"]
                .as_str()
                .map(|at| format!(" · delivered {at}"))
                .unwrap_or_default()
        ));
    }
    if let Some(inbox) = delivery.get("inbox").filter(|row| !row.is_null()) {
        let mut row = vec![format!(
            "inbox {} · {} · from {} · created {}",
            inbox["id"].as_str().unwrap_or("?"),
            inbox["status"].as_str().unwrap_or("?"),
            inbox["from_label"].as_str().unwrap_or("?"),
            inbox["created_at"].as_str().unwrap_or("?")
        )];
        if let Some(expires) = inbox["expires_at"].as_str() {
            row.push(format!("expires {expires}"));
        }
        lines.push(row.join(" · "));
        if let Some(body) = inbox["body"].as_str()
            && !body.is_empty()
        {
            lines.push(format!("  {body}"));
        }
    }
    if let Some(wake) = delivery.get("wake").filter(|row| !row.is_null()) {
        let mut row = vec![format!(
            "wake requested={} created={}",
            wake["requested"].as_bool().unwrap_or(false),
            wake["created"].as_bool().unwrap_or(false)
        )];
        if let Some(continuation) = wake["continuation_id"].as_str() {
            row.push(format!("continuation {continuation}"));
        }
        if let Some(reason) = wake["reason"].as_str()
            && !reason.is_empty()
        {
            row.push(format!("reason {reason}"));
        }
        lines.push(row.join(" · "));
    }
    Some(lines)
}

fn short_hash(value: &str) -> &str {
    match value.char_indices().nth(12) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

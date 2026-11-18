//! Human renderer chunk: status.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(projects) = value.get("projects").and_then(Value::as_array) {
        if projects.is_empty() {
            println!("No projects. Add one with: vp project-add <path>");
        }
        for project in projects {
            println!(
                "{}  {}  [{}]",
                project["id"].as_str().unwrap_or("?"),
                project["name"].as_str().unwrap_or("?"),
                project["default_branch"].as_str().unwrap_or("?")
            );
            println!("  {}", project["root"].as_str().unwrap_or("?"));
        }
        return true;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array) {
        if sessions.is_empty() {
            println!("No sessions.");
        }
        for session in sessions {
            println!(
                "{}  {}  [{}]",
                session["id"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(plan) = value.get("plan").filter(|plan| !plan.is_null()) {
        println!("GOAL  {}", plan["goal"].as_str().unwrap_or("?"));
        for department in plan["departments"].as_array().into_iter().flatten() {
            println!(
                "  {:<16} manager {:<16} profile {}",
                department["name"].as_str().unwrap_or("?"),
                department["manager_title"].as_str().unwrap_or("?"),
                department["profile"].as_str().unwrap_or("?")
            );
        }
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  task {:<12} {:<16} kind {}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(entry) = value.get("entry").filter(|entry| !entry.is_null()) {
        println!(
            "{}  {}  {}",
            entry["status"].as_str().unwrap_or("?"),
            entry["agent_name"].as_str().unwrap_or("?"),
            entry["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = entry["detail"].as_str() {
            println!("{detail}");
        }
        return true;
    }
    if value.get("mirror_events").is_some() && value.get("database_events").is_some() {
        println!(
            "{}  mirror={} database={}  {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["mirror_events"].as_u64().unwrap_or(0),
            value["database_events"].as_u64().unwrap_or(0),
            value["detail"].as_str().unwrap_or("")
        );
        return true;
    }
    if let Some(consistent) = value.get("consistent").and_then(Value::as_bool) {
        println!(
            "{}  events={}  status={} usage={} ledger={} mailbox={} artifacts={} approvals={} plan={} todos={} messages={} agents={} queue={}",
            if consistent { "consistent" } else { "MISMATCH" },
            value["event_count"].as_u64().unwrap_or(0),
            value["status_matches"].as_bool().unwrap_or(false),
            value["usage_matches"].as_bool().unwrap_or(false),
            value["ledger_matches"].as_bool().unwrap_or(false),
            value["mailbox_matches"].as_bool().unwrap_or(false),
            value["artifacts_matches"].as_bool().unwrap_or(false),
            value["approvals_matches"].as_bool().unwrap_or(false),
            value["plan_matches"].as_bool().unwrap_or(false),
            value["todos_matches"].as_bool().unwrap_or(false),
            value["messages_matches"].as_bool().unwrap_or(false),
            value["agents_matches"].as_bool().unwrap_or(false),
            value["queue_matches"].as_bool().unwrap_or(false)
        );
        if value["rebuilt"].as_bool().unwrap_or(false) {
            println!("rebuilt projections from the event stream.");
        } else if value["repaired"].as_bool().unwrap_or(false) {
            println!("repaired projections from the event stream.");
        }
        for mismatch in value["mismatches"].as_array().into_iter().flatten() {
            if let Some(mismatch) = mismatch.as_str() {
                println!("  - {mismatch}");
            }
        }
        return true;
    }
    if let Some(target) = value.get("target_branch").and_then(Value::as_str) {
        println!(
            "landed {} → {} at {}",
            value["branch"].as_str().unwrap_or("?"),
            target,
            value["commit"].as_str().unwrap_or("?")
        );
        if let Some(detail) = value["detail"].as_str() {
            println!("{detail}");
        }
        return true;
    }
    if value.get("removed").is_some() && value.get("bytes_freed").is_some() {
        println!(
            "artifact GC removed {} artifact(s), freed {} bytes",
            value["removed"].as_u64().unwrap_or(0),
            value["bytes_freed"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("Mailbox is empty.");
        }
        for message in messages {
            println!(
                "{}  {} → {}  {}  [{}]",
                message["id"].as_str().unwrap_or("?"),
                message["from_name"].as_str().unwrap_or("?"),
                message["to_name"].as_str().unwrap_or("?"),
                message["kind"].as_str().unwrap_or("?"),
                message["status"].as_str().unwrap_or("?")
            );
            if let Some(body) = message["body"].as_str() {
                println!("  {body}");
            }
        }
        return true;
    }
    if let Some(message) = value.get("message") {
        println!(
            "{} → {}  {}  [{}]",
            message["from_name"].as_str().unwrap_or("?"),
            message["to_name"].as_str().unwrap_or("?"),
            message["kind"].as_str().unwrap_or("?"),
            message["status"].as_str().unwrap_or("?")
        );
        if let Some(body) = message["body"].as_str() {
            println!("{body}");
        }
        return true;
    }
    if let Some(artifacts) = value.get("artifacts").and_then(Value::as_array) {
        if artifacts.is_empty() {
            println!("No artifacts.");
        }
        for artifact in artifacts {
            println!(
                "{}  {:<24} {:<10} {}",
                artifact["id"].as_str().unwrap_or("?"),
                artifact["name"].as_str().unwrap_or("?"),
                artifact["kind"].as_str().unwrap_or("?"),
                artifact["agent_name"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(audit) = value.get("audit")
        && audit.get("messages_with_citations").is_some()
    {
        println!(
            "{} messages with citations · {} without · {} citations · {} sources · dangling {} · unused {}",
            audit["messages_with_citations"].as_u64().unwrap_or(0),
            audit["messages_without_citations"].as_u64().unwrap_or(0),
            audit["total_citations"].as_u64().unwrap_or(0),
            audit["unique_sources"].as_u64().unwrap_or(0),
            audit["dangling_key_count"].as_u64().unwrap_or(0),
            audit["unused_citation_count"].as_u64().unwrap_or(0),
        );
        for message in audit["messages"].as_array().into_iter().flatten() {
            let dangling = message["dangling_keys"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let unused = message["unused_keys"].as_array().map(Vec::len).unwrap_or(0);
            if dangling == 0 && unused == 0 {
                continue;
            }
            println!(
                "  {} · citations {} · dangling {} · unused {}",
                message["message_id"].as_str().unwrap_or("?"),
                message["citation_count"].as_u64().unwrap_or(0),
                message["dangling_keys"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                message["unused_keys"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
            println!(
                "    {}",
                message["excerpt"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\n', " ")
                    .chars()
                    .take(180)
                    .collect::<String>()
            );
        }
        return true;
    }
    if let Some(citations) = value.get("citations").and_then(Value::as_array)
        && citations.iter().all(|item| item.get("citation").is_some())
    {
        if citations.is_empty() {
            println!("No citations.");
            return true;
        }
        for item in citations {
            let citation = &item["citation"];
            println!(
                "[{}] {} · {} · chunk {} · {}",
                citation["key"].as_str().unwrap_or("?"),
                citation["source_title"].as_str().unwrap_or("?"),
                citation["heading"].as_str().unwrap_or(""),
                citation["chunk_no"].as_u64().unwrap_or(0),
                item["message_id"].as_str().unwrap_or("?"),
            );
            println!("  {}", citation["excerpt"].as_str().unwrap_or(""));
        }
        return true;
    }
    if let Some(diff) = value.get("diff") {
        println!(
            "{}  v{} -> v{}  +{} -{} unchanged {}",
            value["name"].as_str().unwrap_or("?"),
            value["from_version"].as_u64().unwrap_or(0),
            value["to_version"].as_u64().unwrap_or(0),
            diff["additions"].as_u64().unwrap_or(0),
            diff["deletions"].as_u64().unwrap_or(0),
            diff["unchanged"].as_u64().unwrap_or(0),
        );
        if diff["identical"].as_bool() == Some(true) {
            println!("identical");
            return true;
        }
        for hunk in diff["hunks"].as_array().into_iter().flatten() {
            println!(
                "@@ -{},{} +{},{} @@",
                hunk["old_start"].as_u64().unwrap_or(0),
                hunk["old_lines"].as_u64().unwrap_or(0),
                hunk["new_start"].as_u64().unwrap_or(0),
                hunk["new_lines"].as_u64().unwrap_or(0)
            );
            for line in hunk["lines"].as_array().into_iter().flatten() {
                let prefix = match line["kind"].as_str() {
                    Some("add") => '+',
                    Some("delete") => '-',
                    _ => ' ',
                };
                let old = line["old_line"]
                    .as_u64()
                    .map(|value| format!("{value:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                let new = line["new_line"]
                    .as_u64()
                    .map(|value| format!("{value:>4}"))
                    .unwrap_or_else(|| "    ".to_string());
                println!(
                    "{old} {new} {prefix} {}",
                    line["text"].as_str().unwrap_or("")
                );
            }
        }
        if diff["truncated"].as_bool() == Some(true) {
            println!("(diff truncated by the line/edit budget)");
        }
        return true;
    }
    if let Some(versions) = value.get("versions").and_then(Value::as_array) {
        if versions.is_empty() {
            println!("No saved versions.");
            return true;
        }
        for version in versions {
            println!(
                "v{}  {}  {} bytes  {}",
                version["version"].as_u64().unwrap_or(0),
                version["name"].as_str().unwrap_or("?"),
                version["size_bytes"].as_u64().unwrap_or(0),
                version["created_at"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(artifact) = value.get("artifact") {
        println!(
            "{}  {}  [{}]  {}  {} bytes",
            artifact["name"].as_str().unwrap_or("?"),
            artifact["kind"].as_str().unwrap_or("?"),
            artifact["mime_type"].as_str().unwrap_or("?"),
            artifact["storage"].as_str().unwrap_or("?"),
            artifact["size_bytes"].as_u64().unwrap_or(0)
        );
        if artifact["storage"].as_str() == Some("file") {
            println!(
                "stored at {}",
                artifact["path"].as_str().unwrap_or("(path unavailable)")
            );
        } else {
            println!("{}", artifact["content"].as_str().unwrap_or(""));
        }
        return true;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        if entries.is_empty() {
            println!("Merge queue is empty.");
        }
        for entry in entries {
            println!(
                "{:<10} {:<10} {:<16} {}",
                entry["status"].as_str().unwrap_or("?"),
                entry["task_id"].as_str().unwrap_or("—"),
                entry["agent_name"].as_str().unwrap_or("?"),
                entry["commit"].as_str().unwrap_or("?")
            );
            if let Some(detail) = entry["detail"].as_str() {
                println!("  {detail}");
            }
        }
        return true;
    }
    if let Some(agents) = value.get("agents").and_then(Value::as_array) {
        if agents.is_empty() {
            println!("No agents.");
        }
        for agent in agents {
            println!(
                "{:<8} {:<16} {:<10} {}",
                agent["role"].as_str().unwrap_or("?"),
                agent["name"].as_str().unwrap_or("?"),
                agent["status"].as_str().unwrap_or("?"),
                agent["id"].as_str().unwrap_or("?")
            );
            if let Some(title) = agent["title"].as_str() {
                println!("         title {title}");
            }
            if let Some(profile) = agent["profile"].as_str() {
                println!("         profile {profile}");
            }
            if let Some(branch) = agent["branch"].as_str() {
                println!("         branch {branch}");
            }
        }
        return true;
    }
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        if items.is_empty() {
            println!("No plan yet.");
        }
        for (index, item) in items.iter().enumerate() {
            let status = item["status"].as_str().unwrap_or("?");
            let marker = match status {
                "completed" => "✓",
                "in_progress" => "•",
                _ => "○",
            };
            println!(
                "{marker} {}. {}  [{}]",
                index + 1,
                item["title"].as_str().unwrap_or("?"),
                status
            );
        }
        return true;
    }
    if let Some(paused) = value.get("paused").and_then(Value::as_bool) {
        println!(
            "{}",
            if paused {
                "paused the active run."
            } else {
                "no active run to pause."
            }
        );
        return true;
    }
    if let Some(resumed) = value.get("resumed").and_then(Value::as_bool) {
        println!(
            "{}",
            if resumed {
                "resumed the active run."
            } else {
                "no paused run to resume."
            }
        );
        return true;
    }
    if let Some(preempted) = value.get("preempted").and_then(Value::as_bool)
        && value.get("records").is_none()
        && value.get("detail").is_some()
    {
        let detail = value
            .get("detail")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if preempted {
            println!("hard preemption signalled.");
        } else {
            println!("no active run was preempted.");
        }
        if !detail.is_empty() {
            println!("  {detail}");
        }
        return true;
    }
    if let Some(cancelled) = value.get("cancelled").and_then(Value::as_bool) {
        println!(
            "{}",
            if cancelled {
                "cancellation requested."
            } else {
                "no active run for that session."
            }
        );
        return true;
    }
    if value.get("discarded").and_then(Value::as_bool) == Some(true) {
        println!("discarded uncommitted changes.");
        return true;
    }
    if let Some(commit) = value.get("commit").and_then(Value::as_str) {
        println!("committed {commit}");
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            println!("  {message}");
        }
        if let Some(summary) = value.get("summary").and_then(Value::as_str) {
            println!("{summary}");
        }
        return true;
    }
    if value.get("patch").is_some() {
        if let Some(status) = value.get("status").and_then(Value::as_str)
            && !status.is_empty()
        {
            println!("{status}\n");
        }
        if let Some(patch) = value.get("patch").and_then(Value::as_str) {
            if patch.is_empty() {
                println!("No changes.");
            } else {
                println!("{patch}");
            }
        }
        return true;
    }
    if let Some(approvals) = value.get("approvals").and_then(Value::as_array) {
        if approvals.is_empty() {
            println!("No approvals.");
        }
        for approval in approvals {
            println!(
                "{}  {}  [{}]  risk={}  waiters={}  priority={}",
                approval["id"].as_str().unwrap_or("?"),
                approval["tool"].as_str().unwrap_or("?"),
                approval["status"].as_str().unwrap_or("?"),
                approval["approval_risk"].as_str().unwrap_or("-"),
                approval["waiter_count"].as_u64().unwrap_or(0),
                approval["priority"].as_i64().unwrap_or(0)
            );
            if let Some(template) = approval["template_id"].as_str() {
                println!("  template {template}");
            }
            if let Some(expires) = approval["expires_at"].as_str() {
                println!("  expires {expires}");
            }
            if let Some(reason) = approval["approval_reason"].as_str() {
                println!("  reason {reason}");
            }
            println!("  {}", approval["input"]);
        }
        return true;
    }
    if let Some(events) = value.get("events").and_then(Value::as_array) {
        if events.is_empty() {
            println!("No events.");
        }
        for event in events {
            println!(
                "#{:04}  {}  {}",
                event["seq"].as_u64().unwrap_or(0),
                event["created_at"].as_str().unwrap_or("?"),
                event["type"].as_str().unwrap_or("?")
            );
            if !event["payload"].is_null() {
                println!("      {}", event["payload"]);
            }
        }
        return true;
    }
    false
}

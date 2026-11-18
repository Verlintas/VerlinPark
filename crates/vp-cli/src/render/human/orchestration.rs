//! Human renderer chunk: orchestration.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(csv) = value.get("csv").and_then(Value::as_str) {
        print!("{csv}");
        return true;
    }
    if let Some(markdown) = value.get("markdown").and_then(Value::as_str) {
        print!("{markdown}");
        return true;
    }
    if let Some(steering) = value.get("steering").and_then(Value::as_object) {
        println!(
            "{}  {}  agent={}  task={}",
            steering["id"].as_str().unwrap_or("?"),
            steering["status"].as_str().unwrap_or("?"),
            steering["agent_id"].as_str().unwrap_or("?"),
            steering["task_id"].as_str().unwrap_or("-")
        );
        println!("  {}", steering["body"].as_str().unwrap_or(""));
        if let Some(delivered_at) = steering["delivered_at"].as_str() {
            println!("  delivered at {delivered_at}");
        }
        return true;
    }
    if let Some(messages) = value.get("steering").and_then(Value::as_array) {
        if messages.is_empty() {
            println!("No steering messages.");
            return true;
        }
        for steering in messages {
            println!(
                "{}  {:<9}  agent={}  task={}  {}",
                steering["id"].as_str().unwrap_or("?"),
                steering["status"].as_str().unwrap_or("?"),
                steering["agent_id"].as_str().unwrap_or("?"),
                steering["task_id"].as_str().unwrap_or("-"),
                steering["created_at"].as_str().unwrap_or("?")
            );
            println!("  {}", steering["body"].as_str().unwrap_or(""));
        }
        return true;
    }
    if value.get("session_id").is_some()
        && let Some(recoveries) = value.get("recoveries").and_then(Value::as_array)
    {
        println!(
            "session {}  recoveries={}",
            value["session_id"].as_str().unwrap_or("?"),
            recoveries.len()
        );
        if recoveries.is_empty() {
            println!("No stall recovery records.");
            return true;
        }
        for recovery in recoveries {
            println!(
                "{}  {:<14}  action={}  attempts={}  last_stall={}",
                recovery["task_id"].as_str().unwrap_or("?"),
                recovery["status"].as_str().unwrap_or("?"),
                recovery["action"].as_str().unwrap_or("?"),
                recovery["attempts"].as_u64().unwrap_or(0),
                recovery["last_stall_count"].as_u64().unwrap_or(0)
            );
            println!(
                "  requested={}  updated={}  next={}",
                recovery["requested_at"].as_str().unwrap_or("-"),
                recovery["updated_at"].as_str().unwrap_or("-"),
                recovery["next_action_at"].as_str().unwrap_or("-")
            );
            if let Some(error) = recovery["last_error"].as_str() {
                println!("  {error}");
            }
        }
        return true;
    }
    if value.get("stalls").is_some() && value.get("progress").is_some() {
        println!(
            "session {}  stall threshold={}s",
            value["session_id"].as_str().unwrap_or("?"),
            value["threshold_secs"].as_u64().unwrap_or(0)
        );
        let progress = value["progress"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if progress.is_empty() {
            println!("No task progress recorded.");
        }
        for task in progress {
            println!(
                "  {}  stalls={}  last={}  turn={}  tool={}",
                task["task_id"].as_str().unwrap_or("?"),
                task["stall_count"].as_u64().unwrap_or(0),
                task["last_progress_at"].as_str().unwrap_or("-"),
                task["last_turn"].as_u64().unwrap_or(0),
                task["last_tool"].as_str().unwrap_or("-")
            );
        }
        let stalls = value["stalls"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        if stalls.is_empty() {
            println!("No stall observations.");
        }
        for stall in stalls {
            println!(
                "  stall {}  age={}s  attempt={}  status={}  observed={}",
                stall["task_id"].as_str().unwrap_or("?"),
                stall["age_secs"].as_u64().unwrap_or(0),
                stall["attempts"].as_u64().unwrap_or(0),
                stall["status"].as_str().unwrap_or("?"),
                stall["observed_at"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(actions) = value.get("actions").and_then(Value::as_array)
        && value.get("session_id").is_some()
        && value.get("snapshot").is_none()
    {
        if actions.is_empty() {
            println!("No orchestration action was needed.");
        } else {
            for action in actions {
                println!(
                    "{}  {}  {}  {}",
                    action["action"].as_str().unwrap_or("?"),
                    action["task_id"].as_str().unwrap_or("-"),
                    action["attempt"].as_u64().unwrap_or(0),
                    action["detail"].as_str().unwrap_or("")
                );
            }
        }
        return true;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && let Some(totals) = value.get("totals")
    {
        println!(
            "{} sessions · {} running · {} attention · {} blocked tasks · {} failed · {} retrying · {} stalled · {} conflicts",
            totals["sessions"].as_u64().unwrap_or(0),
            totals["running"].as_u64().unwrap_or(0),
            totals["attention"].as_u64().unwrap_or(0),
            totals["blocked_tasks"].as_u64().unwrap_or(0),
            totals["failed_tasks"].as_u64().unwrap_or(0),
            totals["retrying_tasks"].as_u64().unwrap_or(0),
            totals["stalled_tasks"].as_u64().unwrap_or(0),
            totals["merge_conflicts"].as_u64().unwrap_or(0)
        );
        for session in sessions {
            println!(
                "  {}  [{}]  {}  r{}  tasks {}/{}  ready {}  blocked {}  failed {}",
                session["project_name"].as_str().unwrap_or("?"),
                session["status"].as_str().unwrap_or("?"),
                session["goal"].as_str().unwrap_or(""),
                session["plan_revision"].as_u64().unwrap_or(0),
                session["metrics"]["completed_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["total_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["ready_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["blocked_tasks"].as_u64().unwrap_or(0),
                session["metrics"]["failed_tasks"].as_u64().unwrap_or(0)
            );
            if let Some(blocker) = session["blockers"]
                .as_array()
                .and_then(|values| values.first())
            {
                println!("      blocker: {}", blocker.as_str().unwrap_or(""));
            }
            if let Some(decision) = session["last_decision"].as_str() {
                println!("      last: {}", decision);
            }
        }
        return true;
    }
    if let Some(snapshot) = value.get("snapshot")
        && snapshot.get("metrics").is_some()
    {
        let metrics = &snapshot["metrics"];
        println!(
            "session {}  status={}  plan={} rev={}",
            snapshot["session_id"].as_str().unwrap_or("?"),
            snapshot["status"].as_str().unwrap_or("?"),
            if snapshot["plan"].is_null() {
                "none"
            } else {
                "present"
            },
            snapshot["plan_revision"].as_u64().unwrap_or(0)
        );
        println!(
            "agents={} running={} queued={} failed={}",
            metrics["total_agents"].as_u64().unwrap_or(0),
            metrics["running_agents"].as_u64().unwrap_or(0),
            metrics["queued_agents"].as_u64().unwrap_or(0),
            metrics["failed_agents"].as_u64().unwrap_or(0)
        );
        println!(
            "tasks={} completed={} active={} ready={} blocked={} failed={} retrying={} stalled={}",
            metrics["total_tasks"].as_u64().unwrap_or(0),
            metrics["completed_tasks"].as_u64().unwrap_or(0),
            metrics["active_tasks"].as_u64().unwrap_or(0),
            metrics["ready_tasks"].as_u64().unwrap_or(0),
            metrics["blocked_tasks"].as_u64().unwrap_or(0),
            metrics["failed_tasks"].as_u64().unwrap_or(0),
            metrics["retrying_tasks"].as_u64().unwrap_or(0),
            metrics["stalled_tasks"].as_u64().unwrap_or(0)
        );
        for task in snapshot["tasks"].as_array().into_iter().flatten() {
            println!(
                "  {} [{}/{}] {} · attempts={} {}",
                task["task_id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["status"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or(""),
                task["attempts"].as_u64().unwrap_or(0),
                task["blocked_by"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        for blocker in snapshot["blockers"].as_array().into_iter().flatten() {
            println!(
                "  blocker {} · {}",
                blocker["kind"].as_str().unwrap_or("?"),
                blocker["message"].as_str().unwrap_or("")
            );
        }
        return true;
    }
    if let Some(revision) = value.get("revision")
        && let Some(plan) = value.get("plan")
    {
        println!(
            "r{}  {}  {}  tasks={}  {}",
            revision["revision"].as_u64().unwrap_or(0),
            revision["kind"].as_str().unwrap_or("?"),
            revision["author"].as_str().unwrap_or("?"),
            revision["task_count"].as_u64().unwrap_or(0),
            revision["reason"].as_str().unwrap_or("")
        );
        println!(
            "diff: +{}  -{}  ~{}",
            revision["added"].as_array().map(Vec::len).unwrap_or(0),
            revision["removed"].as_array().map(Vec::len).unwrap_or(0),
            revision["changed"].as_array().map(Vec::len).unwrap_or(0)
        );
        for task in plan["tasks"].as_array().into_iter().flatten() {
            println!(
                "  {} [{}] {}  deps={}",
                task["id"].as_str().unwrap_or("?"),
                task["department"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or(""),
                task["deps"]
                    .as_array()
                    .map(|deps| deps
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return true;
    }
    if let Some(revisions) = value.get("revisions").and_then(Value::as_array) {
        if revisions.is_empty() {
            println!("No plan revisions.");
        }
        for revision in revisions {
            println!(
                "r{}  {}  {}  tasks={}  +{} -{} ~{}  {}",
                revision["revision"].as_u64().unwrap_or(0),
                revision["kind"].as_str().unwrap_or("?"),
                revision["author"].as_str().unwrap_or("?"),
                revision["task_count"].as_u64().unwrap_or(0),
                revision["added"].as_array().map(Vec::len).unwrap_or(0),
                revision["removed"].as_array().map(Vec::len).unwrap_or(0),
                revision["changed"].as_array().map(Vec::len).unwrap_or(0),
                revision["reason"].as_str().unwrap_or("")
            );
        }
        return true;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
    {
        println!(
            "{}",
            serde_json::to_string_pretty(policy).unwrap_or_default()
        );
        return true;
    }
    if let Some(leases) = value.get("leases").and_then(Value::as_array) {
        println!(
            "runtime {} · {} lease(s)",
            value
                .get("runtime_id")
                .and_then(|value| value.as_str())
                .unwrap_or("?"),
            leases.len()
        );
        if let Some(instance) = value.get("daemon_instance") {
            println!(
                "  instance {}  pid {}  started {}",
                instance
                    .get("instance_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?"),
                instance
                    .get("pid")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                instance
                    .get("started_at")
                    .and_then(|value| value.as_str())
                    .unwrap_or("?")
            );
        }
        for lease in leases {
            println!(
                "  {}  {}  {}  generation {}  {}  expires {}  owner {} ({})",
                lease["kind"].as_str().unwrap_or("?"),
                lease["lease_key"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("-"),
                lease["fencing_token"].as_u64().unwrap_or(0),
                lease["state"].as_str().unwrap_or("?"),
                lease["expires_at"].as_str().unwrap_or("?"),
                lease["owner_id"].as_str().unwrap_or("?"),
                lease["owner_instance"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && entries
            .iter()
            .all(|entry| entry.get("action").is_some() && entry.get("lease_key").is_some())
    {
        if entries.is_empty() {
            println!("No runtime lease history.");
        }
        for entry in entries {
            println!(
                "  {}  {}  {}  generation {}  {} ({})  {}",
                entry["occurred_at"].as_str().unwrap_or("?"),
                entry["action"].as_str().unwrap_or("?"),
                entry["lease_key"].as_str().unwrap_or("?"),
                entry["fencing_token"].as_u64().unwrap_or(0),
                entry["owner_id"].as_str().unwrap_or("?"),
                entry["owner_instance"].as_str().unwrap_or("?"),
                entry["detail"].as_str().unwrap_or("")
            );
        }
        return true;
    }
    if value.get("removed").is_some() {
        println!(
            "Removed {} expired runtime lease(s).",
            value["removed"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if let Some(hits) = value.get("hits").and_then(Value::as_array)
        && value.get("query").is_some()
    {
        if hits.is_empty() {
            println!("No matches.");
            return true;
        }
        for hit in hits {
            let source = hit["source"].as_str().unwrap_or("?");
            let detail = hit["role"]
                .as_str()
                .or_else(|| hit["kind"].as_str())
                .unwrap_or("");
            println!(
                "{} {:<18} {} · {}",
                source,
                detail,
                hit["session_goal"].as_str().unwrap_or("?"),
                hit["snippet"].as_str().unwrap_or("")
            );
            println!(
                "  {} · session {}",
                hit["created_at"].as_str().unwrap_or("?"),
                hit["session_id"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("… more matches exist; increase --limit or narrow the query.");
        }
        return true;
    }
    if let Some(sessions) = value.get("sessions").and_then(Value::as_array)
        && value.get("project_id").is_some()
        && let Some(totals) = value.get("totals")
    {
        println!(
            "project {}  sessions={}  calls={}  in={}  out={}  cache_read={}  cache_write={}  cost=${:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            sessions.len(),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cache_read_tokens"].as_u64().unwrap_or(0),
            totals["cache_write_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for session in sessions {
            println!(
                "  {:<10} calls={:<4} in={:<8} out={:<8} ${:.4}  {}",
                session["status"].as_str().unwrap_or("?"),
                session["calls"].as_u64().unwrap_or(0),
                session["input_tokens"].as_u64().unwrap_or(0),
                session["output_tokens"].as_u64().unwrap_or(0),
                session["cost_usd"].as_f64().unwrap_or(0.0),
                session["goal"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array)
        && tasks
            .first()
            .is_none_or(|task| task.get("attempts").is_some() && task.get("instructions").is_some())
        && value.get("leases").is_none()
        && tasks
            .iter()
            .all(|task| task.get("attempts").is_some() && task.get("instructions").is_some())
    {
        if tasks.is_empty() {
            println!("No intake tasks.");
            return true;
        }
        for task in tasks {
            println!(
                "{:<12} p{:<4} {:<10} {:<9} {}/{}  {}",
                task["status"].as_str().unwrap_or("?"),
                task["priority"].as_i64().unwrap_or(0),
                task["profile"].as_str().unwrap_or("?"),
                task["kind"].as_str().unwrap_or("?"),
                task["attempts"].as_u64().unwrap_or(0),
                task["max_attempts"].as_u64().unwrap_or(0),
                task["title"].as_str().unwrap_or("")
            );
            println!("  id={}", task["id"].as_str().unwrap_or("?"));
            if let Some(session) = task["session_id"].as_str() {
                println!("  session={session}");
            }
            if let Some(deps) = task["deps"].as_array()
                && !deps.is_empty()
            {
                println!(
                    "  depends on: {}",
                    deps.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(error) = task["last_error"].as_str() {
                println!("  last error: {error}");
            } else if let Some(summary) = task["result_summary"].as_str() {
                println!("  result: {summary}");
            }
        }
        return true;
    }
    if let Some(task) = value.get("task")
        && task.get("attempts").is_some()
        && task.get("instructions").is_some()
    {
        println!(
            "{}  p{}  {}  {}/{}  {}",
            task["status"].as_str().unwrap_or("?"),
            task["priority"].as_i64().unwrap_or(0),
            task["profile"].as_str().unwrap_or("?"),
            task["attempts"].as_u64().unwrap_or(0),
            task["max_attempts"].as_u64().unwrap_or(0),
            task["title"].as_str().unwrap_or("")
        );
        println!("id={}", task["id"].as_str().unwrap_or("?"));
        if let Some(session) = task["session_id"].as_str() {
            println!("session={session}");
        }
        if let Some(error) = task["last_error"].as_str() {
            println!("last error: {error}");
        }
        return true;
    }
    if let Some(activations) = value.get("activations").and_then(Value::as_array) {
        if activations.is_empty() {
            println!("No skill activations.");
            return true;
        }
        for activation in activations {
            println!(
                "{} v{}  role={} profile={}  {}",
                activation["name"].as_str().unwrap_or("?"),
                activation["version"].as_str().unwrap_or(""),
                activation["role"].as_str().unwrap_or(""),
                activation["profile"].as_str().unwrap_or(""),
                activation["activated_at"].as_str().unwrap_or("")
            );
            if let Some(allow) = activation["tool_allow"].as_array()
                && !allow.is_empty()
            {
                println!(
                    "  allow: {}",
                    allow
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(deny) = activation["tool_deny"].as_array()
                && !deny.is_empty()
            {
                println!(
                    "  deny: {}",
                    deny.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return true;
    }
    if let Some(skills) = value.get("skills").and_then(Value::as_array) {
        if skills.is_empty() {
            println!("No skills.");
            return true;
        }
        for skill in skills {
            println!(
                "{}  {} {}  [{}]  roles={} profiles={}",
                skill["enabled"].as_bool().unwrap_or(false),
                skill["name"].as_str().unwrap_or("?"),
                skill["version"].as_str().unwrap_or(""),
                skill["source"].as_str().unwrap_or("?"),
                skill["roles"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
                skill["profiles"]
                    .as_array()
                    .map(|values| values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default(),
            );
            println!("  id={}", skill["id"].as_str().unwrap_or("?"));
            if let Some(allow) = skill["tool_allow"].as_array()
                && !allow.is_empty()
            {
                println!(
                    "  allow: {}",
                    allow
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if let Some(deny) = skill["tool_deny"].as_array()
                && !deny.is_empty()
            {
                println!(
                    "  deny: {}",
                    deny.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return true;
    }
    if let Some(skill) = value.get("name").and_then(Value::as_str)
        && value.get("instructions").is_some()
        && value.get("slug").is_some()
    {
        println!(
            "{} {}  enabled={}  id={}",
            skill,
            value["version"].as_str().unwrap_or(""),
            value["enabled"].as_bool().unwrap_or(false),
            value["id"].as_str().unwrap_or("?")
        );
        return true;
    }
    false
}

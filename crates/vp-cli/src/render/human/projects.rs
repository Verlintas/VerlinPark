//! Human renderer chunk: projects.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(task) = value.get("task")
        && task.get("session_id").is_some()
        && task.get("task_id").is_some()
    {
        println!(
            "{} {} -> {}",
            task["session_id"].as_str().unwrap_or("?"),
            task["task_id"].as_str().unwrap_or("?"),
            task["status"].as_str().unwrap_or("?")
        );
        return true;
    }
    if let Some(projects) = value.get("projects").and_then(Value::as_array)
        && value.get("global_session_cap").is_some()
    {
        println!(
            "sessions: active={} queued={} paused={} cap={}",
            value["active_sessions"].as_u64().unwrap_or(0),
            value["queued_sessions"].as_u64().unwrap_or(0),
            value["paused_sessions"].as_u64().unwrap_or(0),
            value["global_session_cap"].as_u64().unwrap_or(0)
        );
        println!(
            "workers: {}  labour: {}  plan tasks: pending={} running={}",
            value["global_worker_cap"].as_u64().unwrap_or(0),
            value["global_labour_cap"].as_u64().unwrap_or(0),
            value["pending_plan_tasks"].as_u64().unwrap_or(0),
            value["running_plan_tasks"].as_u64().unwrap_or(0)
        );
        println!(
            "intake: queued={} active={} failed={}",
            value["queued_intake"].as_u64().unwrap_or(0),
            value["active_intake"].as_u64().unwrap_or(0),
            value["failed_intake"].as_u64().unwrap_or(0)
        );
        for project in projects {
            println!(
                "{}  enabled={} intake={} cap={} cooldown={}s  sessions={} plan={}/{}  intake={}/{}/{}",
                project["project_name"].as_str().unwrap_or("?"),
                project["enabled"].as_bool().unwrap_or(false),
                project["include_intake"].as_bool().unwrap_or(false),
                project["max_intake_parallel"].as_u64().unwrap_or(0),
                project["cooldown_secs"].as_u64().unwrap_or(0),
                project["pending_sessions"].as_u64().unwrap_or(0),
                project["running_plan_tasks"].as_u64().unwrap_or(0),
                project["pending_plan_tasks"].as_u64().unwrap_or(0),
                project["active_intake"].as_u64().unwrap_or(0),
                project["queued_intake"].as_u64().unwrap_or(0),
                project["failed_intake"].as_u64().unwrap_or(0)
            );
        }
        return true;
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array)
        && value.get("leases").is_some()
    {
        if tasks.is_empty() && value["leases"].as_array().is_none_or(Vec::is_empty) {
            println!("No scheduled tasks or write leases.");
            return true;
        }
        for task in tasks {
            println!(
                "{:<10} {:<8} p{:<4} {:<12} {:<24} {}",
                task["status"].as_str().unwrap_or("?"),
                task["task_id"].as_str().unwrap_or("?"),
                task["priority"].as_i64().unwrap_or(0),
                task["department"].as_str().unwrap_or("?"),
                task["session_id"].as_str().unwrap_or("?"),
                task["title"].as_str().unwrap_or("")
            );
            if let Some(error) = task["last_error"].as_str() {
                println!("  last error: {error}");
            }
        }
        for lease in value["leases"].as_array().into_iter().flatten() {
            println!(
                "lease {:<14} {:<20} scopes={}",
                lease["agent_id"].as_str().unwrap_or("?"),
                lease["session_id"].as_str().unwrap_or("?"),
                lease["scopes"]
                    .as_array()
                    .map(|scopes| scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","))
                    .unwrap_or_default()
            );
        }
        return true;
    }
    if let Some(totals) = value.get("totals")
        && value.get("entries").is_some()
    {
        println!(
            "session {}  calls={}  in={}  out={}  cache_read={}  cache_write={}  cost=${:.4}",
            value["session_id"].as_str().unwrap_or("?"),
            totals["calls"].as_u64().unwrap_or(0),
            totals["input_tokens"].as_u64().unwrap_or(0),
            totals["output_tokens"].as_u64().unwrap_or(0),
            totals["cache_read_tokens"].as_u64().unwrap_or(0),
            totals["cache_write_tokens"].as_u64().unwrap_or(0),
            totals["cost_usd"].as_f64().unwrap_or(0.0)
        );
        for (label, field) in [
            ("role", "by_role"),
            ("dept", "by_department"),
            ("task", "by_task"),
            ("agent", "by_agent"),
            ("model", "by_model"),
        ] {
            for bucket in totals[field].as_array().into_iter().flatten() {
                println!(
                    "  {label:5} {:<20} calls={:<3} in={:<7} out={:<7} cache={}/{} ${:.4}",
                    bucket["key"].as_str().unwrap_or("?"),
                    bucket["calls"].as_u64().unwrap_or(0),
                    bucket["input_tokens"].as_u64().unwrap_or(0),
                    bucket["output_tokens"].as_u64().unwrap_or(0),
                    bucket["cache_read_tokens"].as_u64().unwrap_or(0),
                    bucket["cache_write_tokens"].as_u64().unwrap_or(0),
                    bucket["cost_usd"].as_f64().unwrap_or(0.0)
                );
            }
        }
        return true;
    }
    if value.get("session_id").is_some()
        && value.get("worktree").is_some()
        && value.get("commit").is_some()
    {
        println!(
            "rehydrated {} at {} · branch {} · {}",
            value["session_id"].as_str().unwrap_or("?"),
            value["worktree"].as_str().unwrap_or("?"),
            value["branch"].as_str().unwrap_or("?"),
            value["commit"].as_str().unwrap_or("?")
        );
        return true;
    }
    if value.get("session_id").is_some() && value.get("events").is_some() {
        println!(
            "imported session {} into project {} · events={} · status={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            value["events"].as_u64().unwrap_or(0),
            value["status"].as_str().unwrap_or("?")
        );
        return true;
    }
    if value.get("version").is_some() && value.get("exported_at").is_some() {
        println!(
            "session {}  events={}  agents={}  messages={}  queue={}",
            value["session"]["id"].as_str().unwrap_or("?"),
            value["events"].as_array().map_or(0, Vec::len),
            value["agents"].as_array().map_or(0, Vec::len),
            value["messages"].as_array().map_or(0, Vec::len),
            value["merge_queue"].as_array().map_or(0, Vec::len)
        );
        return true;
    }
    if value.get("latency_ms").is_some() && value.get("provider").is_some() {
        println!(
            "provider {} / {}  {} ms  in={} out={}  {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?"),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0),
            value["text"].as_str().unwrap_or("")
        );
        return true;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array)
        && value.get("ok").is_some()
    {
        println!(
            "{}",
            if value["ok"].as_bool().unwrap_or(false) {
                "doctor: ok"
            } else {
                "doctor: errors"
            }
        );
        for check in checks {
            println!(
                "  {:<8} {:<18} {}",
                check["status"].as_str().unwrap_or("?"),
                check["name"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        return true;
    }
    if value.get("budget").is_some() && value.get("spent_tokens").is_some() {
        println!(
            "project {}  token-budget={}  cost-budget={}  spent-tokens={}  spent-usd={:.4}",
            value["project_id"].as_str().unwrap_or("?"),
            value["budget"]["token_budget"].as_u64().unwrap_or(0),
            value["budget"]["cost_budget_usd"].as_f64().unwrap_or(0.0),
            value["spent_tokens"].as_u64().unwrap_or(0),
            value["spent_cost_usd"].as_f64().unwrap_or(0.0)
        );
        return true;
    }
    if value.get("scanned").is_some() && value.get("entries").is_some() {
        println!(
            "project {}  scanned={}  removed={}  skipped={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["removed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        for entry in value["entries"].as_array().into_iter().flatten() {
            println!(
                "{}  {}  {}  {}",
                if entry["removed"].as_bool().unwrap_or(false) {
                    "removed"
                } else {
                    "skipped"
                },
                entry["session_id"].as_str().unwrap_or("?"),
                entry["branch"].as_str().unwrap_or("?"),
                entry["reason"].as_str().unwrap_or("")
            );
        }
        return true;
    }
    if value.get("artifact_id").is_some() && value.get("path").is_some() {
        println!(
            "materialized {} at {}",
            value["artifact_id"].as_str().unwrap_or("?"),
            value["path"].as_str().unwrap_or("?")
        );
        return true;
    }
    if value.get("recent_decisions").is_some() && value.get("contract").is_some() {
        let contract = &value["contract"];
        println!(
            "session {}  project {}  policy={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["project_id"].as_str().unwrap_or("?"),
            value["policy"]["mode"].as_str().unwrap_or("enforce")
        );
        if contract.is_null() {
            println!("no task contract derived yet");
        } else {
            println!(
                "contract {}  role={}  task={}  revision={}",
                contract["hash"].as_str().unwrap_or("?"),
                contract["role"].as_str().unwrap_or("?"),
                contract["task_id"].as_str().unwrap_or("session"),
                contract["plan_revision"].as_u64().unwrap_or(0)
            );
            println!(
                "paths={} forbidden={} acceptance={} egress={} tools={} scope={} mode={}",
                contract["allowed_paths_count"].as_u64().unwrap_or(0),
                contract["forbidden_paths_count"].as_u64().unwrap_or(0),
                contract["acceptance_count"].as_u64().unwrap_or(0),
                contract["egress"].as_str().unwrap_or("?"),
                contract["allowed_tools_count"].as_u64().unwrap_or(0),
                if contract["unrestricted_scope"].as_bool().unwrap_or(false) {
                    "unrestricted"
                } else {
                    "declared"
                },
                contract["enforcement_mode"].as_str().unwrap_or("?")
            );
            if let Some(budget) = contract.get("max_writes").filter(|v| !v.is_null()) {
                println!(
                    "mutation used writes={}/{} bytes={}/{} commits={}/{}",
                    contract["writes_used"].as_u64().unwrap_or(0),
                    budget.as_u64().unwrap_or(0),
                    contract["bytes_used"].as_u64().unwrap_or(0),
                    contract["max_bytes"].as_u64().unwrap_or(0),
                    contract["commits_used"].as_u64().unwrap_or(0),
                    contract["max_commits"].as_u64().unwrap_or(0)
                );
            }
        }
        for decision in value["recent_decisions"].as_array().into_iter().flatten() {
            println!(
                "  #{} {}  {}  {}  {}  risk={}",
                decision["seq"].as_u64().unwrap_or(0),
                decision["decision"].as_str().unwrap_or("?"),
                decision["tool"].as_str().unwrap_or("?"),
                decision["rule"].as_str().unwrap_or("?"),
                decision["reason"].as_str().unwrap_or(""),
                decision["risk"].as_str().unwrap_or("?")
            );
        }
        return true;
    }
    if value.get("policy").is_some() && value["policy"].get("enforce_shell").is_some() {
        let policy = &value["policy"];
        println!(
            "project {}  contract-policy enabled={} mode={} enforce_shell={} max_writes={:?} max_bytes={:?} max_commits={:?}",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(true),
            policy["mode"].as_str().unwrap_or("enforce"),
            policy["enforce_shell"].as_bool().unwrap_or(false),
            policy["max_writes"].as_u64(),
            policy["max_bytes"].as_u64(),
            policy["max_commits"].as_u64()
        );
        for path in policy["forbidden_paths"].as_array().into_iter().flatten() {
            println!("  forbidden {}", path.as_str().unwrap_or("?"));
        }
        for host in policy["egress_allowlist"].as_array().into_iter().flatten() {
            println!("  egress {}", host.as_str().unwrap_or("?"));
        }
        return true;
    }
    if value.get("pins").is_some() && value.get("pinned").is_some() {
        println!(
            "project {}  pins={} drift={} unverified={} removed={}",
            value["project_id"].as_str().unwrap_or("?"),
            value["pinned"].as_u64().unwrap_or(0),
            value["drift"].as_u64().unwrap_or(0),
            value["unverified"].as_u64().unwrap_or(0),
            value["removed"].as_u64().unwrap_or(0)
        );
        for pin in value["pins"].as_array().into_iter().flatten() {
            println!(
                "  {:<11} {} / {}  schema={} description={}",
                pin["status"].as_str().unwrap_or("?"),
                pin["server_name"].as_str().unwrap_or("?"),
                pin["tool_name"].as_str().unwrap_or("?"),
                pin["observed_schema_hash"]
                    .as_str()
                    .or_else(|| pin["schema_hash"].as_str())
                    .unwrap_or("?"),
                pin["observed_description_hash"]
                    .as_str()
                    .or_else(|| pin["description_hash"].as_str())
                    .unwrap_or("?")
            );
        }
        return true;
    }
    if value.get("pin").is_some() && value.get("message").is_some() {
        println!(
            "{}  status={}",
            value["message"].as_str().unwrap_or("MCP pin action"),
            value["status"].as_str().unwrap_or("?")
        );
        if let Some(pin) = value.get("pin").filter(|pin| !pin.is_null()) {
            println!(
                "  {} / {}  schema={} description={} approved_by={}",
                pin["server_name"].as_str().unwrap_or("?"),
                pin["tool_name"].as_str().unwrap_or("?"),
                pin["schema_hash"].as_str().unwrap_or("?"),
                pin["description_hash"].as_str().unwrap_or("?"),
                pin["approved_by"].as_str().unwrap_or("-")
            );
        }
        return true;
    }
    if let Some(servers) = value.get("servers").and_then(Value::as_array) {
        for server in servers {
            let transport = server["transport"].as_str().unwrap_or("stdio");
            let target = server["url"]
                .as_str()
                .or_else(|| server["command"].as_str())
                .unwrap_or("?");
            println!(
                "{}  {}  {}  {}  enabled={}",
                server["id"].as_str().unwrap_or("?"),
                server["name"].as_str().unwrap_or("?"),
                transport,
                target,
                server["enabled"].as_bool().unwrap_or(false)
            );
        }
        return true;
    }
    false
}

//! Human renderer chunk: memory.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if value.get("answered").is_some() && value.get("started").is_none() {
        println!(
            "question answered={}",
            value["answered"].as_bool().unwrap_or(false)
        );
        return true;
    }
    if let Some(sha256) = value.get("sha256").and_then(Value::as_str)
        && value.get("size").is_some()
        && value.get("path").is_some()
        && value.get("content").is_none()
    {
        println!(
            "wrote {} ({} bytes, sha256 {})",
            value["path"].as_str().unwrap_or("?"),
            value["size"].as_u64().unwrap_or(0),
            &sha256[..sha256.len().min(12)]
        );
        return true;
    }
    if value.get("attempts").is_some() && value.get("instructions").is_some() {
        println!(
            "{}  p{}  {}  {}/{}  {}",
            value["status"].as_str().unwrap_or("?"),
            value["priority"].as_i64().unwrap_or(0),
            value["profile"].as_str().unwrap_or("?"),
            value["attempts"].as_u64().unwrap_or(0),
            value["max_attempts"].as_u64().unwrap_or(0),
            value["title"].as_str().unwrap_or("")
        );
        println!("id={}", value["id"].as_str().unwrap_or("?"));
        if let Some(session) = value["session_id"].as_str() {
            println!("session={session}");
        }
        if let Some(error) = value["last_error"].as_str() {
            println!("last error: {error}");
        }
        return true;
    }
    if let Some(priority) = value.get("priority").and_then(Value::as_i64)
        && value.get("session_id").is_some()
        && value.get("commits").is_none()
    {
        println!(
            "session {} priority={}",
            value["session_id"].as_str().unwrap_or("?"),
            priority
        );
        return true;
    }
    if let Some(commits) = value.get("commits").and_then(Value::as_array) {
        for commit in commits {
            println!(
                "{}  {}  {}  {}",
                commit["short_hash"].as_str().unwrap_or("?"),
                commit["author_date"].as_str().unwrap_or("?"),
                commit["author"].as_str().unwrap_or("?"),
                commit["subject"].as_str().unwrap_or("?")
            );
            if let Some(refs) = commit["refs"].as_str()
                && !refs.is_empty()
            {
                println!("         {}", refs);
            }
        }
        return true;
    }
    if let Some(output) = value.get("output").and_then(Value::as_str)
        && value.get("commit").is_some()
    {
        println!("{output}");
        return true;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array) {
        for entry in entries {
            println!(
                "{:<4} {:>10}  {}",
                entry["kind"].as_str().unwrap_or("?"),
                entry["size"].as_u64().unwrap_or(0),
                entry["path"].as_str().unwrap_or("?")
            );
        }
        if value["truncated"].as_bool().unwrap_or(false) {
            println!("(truncated)");
        }
        return true;
    }
    if let Some(content) = value.get("content").and_then(Value::as_str)
        && value.get("size").is_some()
        && value.get("binary").is_some()
    {
        print!("{content}");
        return true;
    }
    if let Some(text) = value.get("text").and_then(Value::as_str)
        && let Some(memories) = value.get("memories").and_then(Value::as_array)
    {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        if !memories.is_empty() {
            let ids = memories
                .iter()
                .filter_map(|memory| memory.get("memory"))
                .filter_map(|memory| memory["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("selected: {ids}");
        }
        return true;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array)
        && memories.iter().any(|memory| memory.get("score").is_some())
    {
        for matched in memories {
            let memory = &matched["memory"];
            let reasons = matched["reasons"]
                .as_array()
                .map(|reasons| {
                    reasons
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            println!(
                "{:>8.3} [{}·{}] {} · {} source={}{}{}",
                matched["score"].as_f64().unwrap_or(0.0),
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["id"].as_str().unwrap_or("?"),
                matched["source"].as_str().unwrap_or("lexical"),
                matched["lexical_score"]
                    .as_f64()
                    .map(|score| format!(" lexical={score:.3}"))
                    .unwrap_or_default(),
                matched["semantic_score"]
                    .as_f64()
                    .map(|score| format!(" semantic={score:.3}"))
                    .unwrap_or_default()
            );
            println!("  reasons: {reasons}");
            if let Some(reason) = matched["semantic_unavailable"].as_str() {
                println!("  semantic_unavailable: {reason}");
            }
            println!("  {}", memory["body"].as_str().unwrap_or(""));
        }
        return true;
    }
    if let Some(checks) = value.get("checks").and_then(Value::as_array)
        && let Some(memory) = value.get("memory")
    {
        for check in checks {
            println!(
                "{} {} · {} · {}",
                check["status"].as_str().unwrap_or("unchecked"),
                check["evidence"]["kind"].as_str().unwrap_or("?"),
                check["evidence"]["reference"].as_str().unwrap_or("?"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        println!(
            "memory {} status={} verified_at={} stale_reason={}",
            memory["id"].as_str().unwrap_or("?"),
            memory["status"].as_str().unwrap_or("active"),
            memory["last_verified_at"].as_str().unwrap_or("never"),
            memory["stale_reason"].as_str().unwrap_or("none")
        );
        return true;
    }
    if let Some(conflicts) = value.get("conflicts").and_then(Value::as_array) {
        if let Some(detected) = value.get("detected").and_then(Value::as_u64) {
            println!(
                "memory conflict scan: detected={} open={}",
                detected,
                value["open"].as_u64().unwrap_or(0)
            );
        }
        if conflicts.is_empty() {
            println!("no memory conflicts");
        }
        for conflict in conflicts {
            println!(
                "[{}·{}] {} · {} · {}",
                conflict["memory_a_kind"].as_str().unwrap_or("?"),
                conflict["memory_b_kind"].as_str().unwrap_or("?"),
                conflict["memory_a_title"].as_str().unwrap_or("?"),
                conflict["memory_b_title"].as_str().unwrap_or("?"),
                conflict["status"].as_str().unwrap_or("open")
            );
            println!(
                "  {} {} · id {}",
                conflict["scope_kind"].as_str().unwrap_or("?"),
                conflict["scope_value"].as_str().unwrap_or("?"),
                conflict["id"].as_str().unwrap_or("?")
            );
            println!("  {}", conflict["reason"].as_str().unwrap_or(""));
            for side in ["a", "b"] {
                let key = format!("memory_{side}_excerpt");
                if let Some(excerpt) = conflict[&key].as_str()
                    && !excerpt.is_empty()
                {
                    println!("  {}: {}", side.to_uppercase(), excerpt);
                }
            }
        }
        return true;
    }
    if let Some(conflict) = value.get("conflict") {
        println!(
            "memory conflict {} resolved={} action={} at={}",
            conflict["id"].as_str().unwrap_or("?"),
            conflict["status"].as_str().unwrap_or("?"),
            conflict["resolution"].as_str().unwrap_or("?"),
            conflict["resolved_at"].as_str().unwrap_or("?")
        );
        if let Some(note) = conflict["note"].as_str() {
            println!("  note: {note}");
        }
        if let Some(memories) = value.get("memories").and_then(Value::as_array) {
            for memory in memories {
                println!(
                    "  [{}·{}] {} · {} · {}",
                    memory["kind"].as_str().unwrap_or("?"),
                    memory["source"].as_str().unwrap_or("agent"),
                    memory["title"].as_str().unwrap_or("?"),
                    memory["status"].as_str().unwrap_or("active"),
                    memory["id"].as_str().unwrap_or("?")
                );
            }
        }
        return true;
    }
    if value.get("indexable").is_some() && value.get("coverage").is_some() {
        println!(
            "memory embeddings: enabled={} configured={} model={} dimensions={}",
            value["enabled"].as_bool().unwrap_or(false),
            value["configured"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0)
        );
        println!(
            "  coverage={:.1}% indexed={}/{} pending={} failed={} skipped={}",
            value["coverage"].as_f64().unwrap_or(0.0) * 100.0,
            value["indexed"].as_u64().unwrap_or(0),
            value["indexable"].as_u64().unwrap_or(0),
            value["pending"].as_u64().unwrap_or(0),
            value["failed"].as_u64().unwrap_or(0),
            value["skipped"].as_u64().unwrap_or(0)
        );
        if let Some(error) = value["last_error"].as_str() {
            println!("  last_error: {error}");
        }
        if let Some(usage) = value.get("usage") {
            println!(
                "  embedding_calls={} input_tokens={} estimated_tokens={} bytes={} provider_reported_calls={}",
                usage["calls"].as_u64().unwrap_or(0),
                usage["input_tokens"].as_u64().unwrap_or(0),
                usage["estimated_tokens"].as_u64().unwrap_or(0),
                usage["bytes"].as_u64().unwrap_or(0),
                usage["provider_reported_calls"].as_u64().unwrap_or(0)
            );
        }
        if let Some(jobs) = value.get("recent_jobs").and_then(Value::as_array) {
            for job in jobs.iter().take(10) {
                println!(
                    "  {} {} attempts={} updated={}{}",
                    job["status"].as_str().unwrap_or("?"),
                    job["title"].as_str().unwrap_or("?"),
                    job["attempts"].as_u64().unwrap_or(0),
                    job["updated_at"].as_str().unwrap_or("?"),
                    job["last_error"]
                        .as_str()
                        .map(|error| format!(" · {error}"))
                        .unwrap_or_default()
                );
            }
        }
        return true;
    }
    if value.get("cleared").is_some() && value.get("enqueued").is_some() {
        println!(
            "memory embeddings rebuild: cleared={} enqueued={} model={} dimensions={} enabled={}",
            value["cleared"].as_u64().unwrap_or(0),
            value["enqueued"].as_u64().unwrap_or(0),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["enabled"].as_bool().unwrap_or(false)
        );
        return true;
    }
    if value.get("ok").is_some() && value.get("latency_ms").is_some() {
        println!(
            "memory embeddings test: ok={} model={} dimensions={} latency_ms={} input_tokens={} estimated_tokens={} provider_reported={}",
            value["ok"].as_bool().unwrap_or(false),
            value["model"].as_str().unwrap_or("none"),
            value["dimensions"].as_u64().unwrap_or(0),
            value["latency_ms"].as_u64().unwrap_or(0),
            value["input_tokens"].as_u64().unwrap_or(0),
            value["estimated_tokens"].as_u64().unwrap_or(0),
            value["provider_reported"].as_bool().unwrap_or(false)
        );
        if let Some(error) = value["error"].as_str() {
            println!("  error: {error}");
        }
        return true;
    }
    if value.get("verification_due").is_some() && value.get("evidence_covered").is_some() {
        println!(
            "memory health: total={} active={} stale={} superseded={} archived={} pinned={}",
            value["total"].as_u64().unwrap_or(0),
            value["active"].as_u64().unwrap_or(0),
            value["stale"].as_u64().unwrap_or(0),
            value["superseded"].as_u64().unwrap_or(0),
            value["archived"].as_u64().unwrap_or(0),
            value["pinned"].as_u64().unwrap_or(0)
        );
        println!(
            "  evidence_covered={} verified={} verification_due={} open_conflicts={}",
            value["evidence_covered"].as_u64().unwrap_or(0),
            value["verified"].as_u64().unwrap_or(0),
            value["verification_due"].as_u64().unwrap_or(0),
            value["open_conflicts"].as_u64().unwrap_or(0)
        );
        if let Some(kinds) = value.get("by_kind").and_then(Value::as_array) {
            let counts = kinds
                .iter()
                .map(|count| {
                    format!(
                        "{}={}",
                        count["key"].as_str().unwrap_or("?"),
                        count["count"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("  by_kind: {counts}");
        }
        return true;
    }
    if value.get("by_status").and_then(Value::as_array).is_some() && value.get("total").is_some() {
        println!(
            "memory stats: total={}",
            value["total"].as_u64().unwrap_or(0)
        );
        for key in ["by_status", "by_source", "by_kind"] {
            let counts = value[key]
                .as_array()
                .map(|counts| {
                    counts
                        .iter()
                        .map(|count| {
                            format!(
                                "{}={}",
                                count["key"].as_str().unwrap_or("?"),
                                count["count"].as_u64().unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            println!("  {key}: {counts}");
        }
        return true;
    }
    if value.get("marked_stale").is_some() && value.get("candidates").is_some() {
        println!(
            "memory sweep: candidates={} marked_stale={} dry_run={}",
            value["candidates"].as_u64().unwrap_or(0),
            value["marked_stale"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        );
        if let Some(ids) = value.get("memory_ids").and_then(Value::as_array)
            && !ids.is_empty()
        {
            println!(
                "  ids: {}",
                ids.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        return true;
    }
    if let Some(memories) = value.get("memories").and_then(Value::as_array) {
        for memory in memories {
            println!(
                "[{}·{}] {} · {} · {}",
                memory["kind"].as_str().unwrap_or("note"),
                memory["source"].as_str().unwrap_or("agent"),
                memory["title"].as_str().unwrap_or("?"),
                memory["status"].as_str().unwrap_or("active"),
                memory["id"].as_str().unwrap_or("?")
            );
            println!("  {}", memory["body"].as_str().unwrap_or(""));
        }
        return true;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && policy.get("cooldown_secs").is_some()
    {
        println!(
            "project {}  task-scheduler enabled={} cooldown={}s",
            value["project_id"].as_str().unwrap_or("?"),
            policy["enabled"].as_bool().unwrap_or(false),
            policy["cooldown_secs"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if let Some(policy) = value.get("policy")
        && value.get("project_id").is_some()
        && policy.get("enabled").is_some()
    {
        println!(
            "memory policy enabled={} max_items={} max_age_days={} task_result_verify_days={} default_verify_days={} auto_supersede={} brief_max_chars={} sweep_stale={}",
            policy["enabled"].as_bool().unwrap_or(false),
            policy["max_items"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["max_age_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["task_result_verify_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["default_verify_days"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["auto_supersede"].as_bool().unwrap_or(true),
            policy["brief_max_chars"]
                .as_u64()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            policy["sweep_stale"].as_bool().unwrap_or(false)
        );
        return true;
    }
    false
}

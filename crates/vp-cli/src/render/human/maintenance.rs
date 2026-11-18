//! Human renderer chunk: maintenance.

use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(candidates) = value.get("candidates").and_then(Value::as_u64) {
        println!(
            "memory prune: candidates={} removed={} dry_run={}",
            candidates,
            value["removed"].as_u64().unwrap_or(0),
            value["dry_run"].as_bool().unwrap_or(false)
        );
        return true;
    }
    if let Some(memory) = value.get("memory") {
        println!(
            "[{}·{}] {} · {} · status={} source={}",
            memory["kind"].as_str().unwrap_or("note"),
            memory["source"].as_str().unwrap_or("agent"),
            memory["title"].as_str().unwrap_or("?"),
            memory["id"].as_str().unwrap_or("?"),
            memory["status"].as_str().unwrap_or("active"),
            memory["source"].as_str().unwrap_or("agent")
        );
        return true;
    }
    if value.get("dry_run").is_some() && value.get("projects").is_some() {
        println!(
            "db rebuild{}: projects={} sessions={} events={} backup={}",
            if value["dry_run"].as_bool().unwrap_or(false) {
                " (dry run)"
            } else {
                ""
            },
            value["projects"].as_u64().unwrap_or(0),
            value["sessions"].as_u64().unwrap_or(0),
            value["events"].as_u64().unwrap_or(0),
            value["backup"].as_str().unwrap_or("none")
        );
        for warning in value["skipped"].as_array().into_iter().flatten() {
            println!("  skipped: {}", warning.as_str().unwrap_or("?"));
        }
        return true;
    }
    if let Some(started) = value.get("started").and_then(Value::as_bool) {
        println!(
            "session {}  plan-resume-started={}",
            value["session_id"].as_str().unwrap_or("?"),
            started
        );
        return true;
    }
    if value.get("session_id").is_some() && value.get("backfilled").is_some() {
        println!(
            "session {}  scanned={}  backfilled={}",
            value["session_id"].as_str().unwrap_or("?"),
            value["scanned"].as_u64().unwrap_or(0),
            value["backfilled"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if value.get("hash_mismatches").is_some() && value.get("orphan_blobs").is_some() {
        println!(
            "tool-output audit: {} row(s), {} blob(s), {} missing, {} mismatched, {} unsafe, {} orphan(s) ({} bytes, cleaned {})",
            value["rows"].as_u64().unwrap_or(0),
            value["blob_files"].as_u64().unwrap_or(0),
            value["missing_blobs"].as_u64().unwrap_or(0),
            value["hash_mismatches"].as_u64().unwrap_or(0),
            value["unsafe_blob_files"].as_u64().unwrap_or(0),
            value["orphan_blobs"].as_u64().unwrap_or(0),
            value["orphan_bytes"].as_u64().unwrap_or(0),
            value["cleaned_orphans"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && value.get("total").is_some()
    {
        if entries.is_empty() {
            println!("No spilled tool outputs.");
        } else {
            for entry in entries {
                println!(
                    "{}  {:<12}  {:>8}B  {} lines  {}",
                    entry["id"].as_str().unwrap_or("?"),
                    entry["tool_name"].as_str().unwrap_or("?"),
                    entry["bytes"].as_u64().unwrap_or(0),
                    entry["lines"].as_u64().unwrap_or(0),
                    entry["created_at"].as_str().unwrap_or("?")
                );
            }
            println!(
                "{} of {} handle(s) shown.",
                entries.len(),
                value["total"].as_u64().unwrap_or(0)
            );
        }
        return true;
    }
    if value.get("entries").is_some_and(Value::is_u64)
        && value
            .get("policy")
            .is_some_and(|policy| policy.get("spill_threshold_bytes").is_some())
    {
        println!(
            "tool outputs: {} handle(s), {} bytes stored, {} active, {} expired",
            value["entries"].as_u64().unwrap_or(0),
            value["total_bytes"].as_u64().unwrap_or(0),
            value["active_entries"].as_u64().unwrap_or(0),
            value["expired_entries"].as_u64().unwrap_or(0)
        );
        return true;
    }
    if let Some(policy) = value.get("policy") {
        if policy.get("spill_threshold_bytes").is_some() {
            println!(
                "project {}  tool-output={}  spill>{}  preview={}  turn-budget={}  retention={}s  max-entries={}",
                value["project_id"].as_str().unwrap_or("?"),
                if policy["enabled"].as_bool().unwrap_or(true) {
                    "enabled"
                } else {
                    "disabled"
                },
                policy["spill_threshold_bytes"].as_u64().unwrap_or(0),
                policy["preview_bytes"].as_u64().unwrap_or(0),
                policy["max_total_bytes_per_turn"].as_u64().unwrap_or(0),
                policy["retention_secs"].as_u64().unwrap_or(0),
                policy["max_entries"].as_u64().unwrap_or(0)
            );
            return true;
        }
        if policy.get("delete_unread").is_some() {
            println!(
                "project {}  mailbox-max-age={}  mailbox-max-count={}  delete-unread={}  auto-gc={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0),
                policy["delete_unread"].as_bool().unwrap_or(false),
                policy["auto_gc"].as_bool().unwrap_or(false)
            );
            return true;
        }
        if policy.get("auto_gc").is_some() {
            println!(
                "project {}  auto-gc={}  max-bytes={}  max-age={}  max-count={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_gc"].as_bool().unwrap_or(false),
                policy["max_bytes"].as_u64().unwrap_or(0),
                policy["max_age_secs"].as_u64().unwrap_or(0),
                policy["max_count"].as_u64().unwrap_or(0)
            );
            return true;
        }
        if policy.get("enabled").is_some() && policy.get("older_than_secs").is_some() {
            println!(
                "project {}  auto-cleanup={}  older-than={}d  delete-branches={}  keep-failed={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["enabled"].as_bool().unwrap_or(false),
                policy["older_than_secs"].as_u64().unwrap_or(0) / 86_400,
                policy["delete_branches"].as_bool().unwrap_or(false),
                policy["keep_failed"].as_bool().unwrap_or(true)
            );
            return true;
        }
        if policy.get("auto_resolve").is_some() {
            println!(
                "project {}  auto-resolve={}  max-attempts={}  max-tokens={}",
                value["project_id"].as_str().unwrap_or("?"),
                policy["auto_resolve"].as_bool().unwrap_or(false),
                policy["max_auto_attempts"].as_u64().unwrap_or(0),
                policy["max_resolve_tokens"]
                    .as_u64()
                    .map_or_else(|| "session budget".to_string(), |value| value.to_string())
            );
            return true;
        }
        println!(
            "project {}  network={}  restrict-home={}  read-jail={}",
            value["project_id"].as_str().unwrap_or("?"),
            policy["network"].as_str().unwrap_or("?"),
            policy["restrict_home"].as_bool().unwrap_or(false),
            policy["read_jail"].as_bool().unwrap_or(false)
        );
        let paths = policy["read_deny"].as_array();
        if paths.is_none_or(|paths| paths.is_empty()) {
            println!("read-deny: none");
        } else {
            for path in paths.into_iter().flatten() {
                println!("read-deny: {}", path.as_str().unwrap_or("?"));
            }
        }
        return true;
    }
    false
}

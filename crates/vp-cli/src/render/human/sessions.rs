//! Human renderer chunk: sessions.

use crate::render::instructions::*;
use crate::render::support::*;
use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if value.as_object().is_some_and(|object| {
        object.len() == 2
            && object.contains_key("session_id")
            && object.contains_key("continuations")
    }) && let Some(continuations) = value.get("continuations").and_then(Value::as_array)
    {
        if continuations.is_empty() {
            println!("No continuations.");
            return true;
        }
        for continuation in continuations {
            let id = continuation["id"].as_str().unwrap_or("?");
            let series = continuation["series_id"]
                .as_str()
                .filter(|series| !series.is_empty())
                .unwrap_or(id);
            let run_count = continuation["run_count"].as_u64().unwrap_or(0);
            let max_runs = continuation["recurrence_max_runs"].as_u64().unwrap_or(1);
            let mut header = format!(
                "{id}  {}  run {run_count}/{max_runs}  series {series}",
                continuation["status"].as_str().unwrap_or("?")
            );
            if let Some(every_secs) = continuation["recurrence_every_secs"].as_u64() {
                header.push_str(&format!("  every {every_secs}s"));
            }
            println!("{header}");
            if let Some(deadline) = continuation["deadline_at"].as_str() {
                println!("  deadline {deadline}");
            }
            let watch = continuation["watch_task_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if !watch.is_empty() {
                println!(
                    "  watch {}: {watch}",
                    continuation["watch_mode"].as_str().unwrap_or("all")
                );
            }
            if let Some(error) = continuation["last_error"].as_str() {
                println!("  {error}");
            }
            println!("  {}", continuation["body"].as_str().unwrap_or(""));
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("goal").is_none()
        && let Some(unlimited) = value.get("unlimited").and_then(Value::as_bool)
    {
        println!(
            "session {} budget mode: {}",
            session_id,
            if unlimited { "unlimited" } else { "limited" }
        );
        return true;
    }
    if value.get("console_streamed").and_then(Value::as_bool) == Some(true)
        || value.get("terminal_streamed").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    if let Some(project_id) = value.get("project_id").and_then(Value::as_str)
        && value
            .get("sessions_observed")
            .and_then(Value::as_u64)
            .is_some()
        && value.get("scan_limit").and_then(Value::as_u64).is_some()
    {
        println!(
            "project cache  {project_id}  {} session(s) · {} call(s) · scan limit {}{}",
            value["sessions_observed"].as_u64().unwrap_or(0),
            value["calls"].as_u64().unwrap_or(0),
            value["scan_limit"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        );
        println!(
            "  hit ratio        {:.2}% · read {} · miss {} · write {}",
            value["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_miss_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0)
        );
        if let (Some(p50), Some(p95)) = (
            value["hit_ratio_p50"].as_f64(),
            value["hit_ratio_p95"].as_f64(),
        ) {
            println!(
                "  percentiles      p50 {:.2}% · p95 {:.2}%",
                p50 * 100.0,
                p95 * 100.0
            );
        }
        println!(
            "  continuity       append-only {} · breaks {} · epochs {}",
            value["append_only_calls"].as_u64().unwrap_or(0),
            value["append_only_breaks"].as_u64().unwrap_or(0),
            value["cache_epochs"].as_u64().unwrap_or(0)
        );
        if let Some(reasons) = value["top_miss_reasons"].as_array()
            && !reasons.is_empty()
        {
            let rendered = reasons
                .iter()
                .map(|reason| {
                    format!(
                        "{}×{}",
                        reason["key"].as_str().unwrap_or("?"),
                        reason["calls"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            println!("  top misses       {rendered}");
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("returned").and_then(Value::as_u64).is_some()
        && value.get("calls").and_then(Value::as_array).is_some()
    {
        println!(
            "cache calls  {session_id}  {} returned · {} matching · limit {}{}",
            value["returned"].as_u64().unwrap_or(0),
            value["observed"].as_u64().unwrap_or(0),
            value["limit"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        );
        if let Some(role) = value["role"].as_str() {
            println!("  role filter     {role}");
        }
        if let Some(reason) = value["miss_reason"].as_str() {
            println!("  reason filter   {reason}");
        }
        println!(
            "  {:<19} {:<14} {:>8} {:>8} {:>8} {:>7} {:>7} {:<16} EPOCH",
            "AT", "ROLE", "INPUT", "READ", "MISS", "HIT", "APPEND", "MISS REASON"
        );
        for call in value["calls"].as_array().into_iter().flatten() {
            let append = match call["append_only"].as_bool() {
                Some(true) => "yes",
                Some(false) => "break",
                None => "n/a",
            };
            println!(
                "  {:<19} {:<14} {:>8} {:>8} {:>8} {:>7.2}% {:>7} {:<16} {}",
                call["at"].as_str().unwrap_or("-"),
                call["role"].as_str().unwrap_or("-"),
                call["input_tokens"].as_u64().unwrap_or(0),
                call["cache_read_tokens"].as_u64().unwrap_or(0),
                call["cache_miss_tokens"].as_u64().unwrap_or(0),
                call["cache_hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                append,
                call["miss_reason"].as_str().unwrap_or("-"),
                short_hash(&call["cache_epoch"])
            );
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("calls_checked").and_then(Value::as_u64).is_some()
        && value.get("findings").and_then(Value::as_array).is_some()
    {
        println!("cache audit  {session_id}");
        println!(
            "  calls            {} agent · {} warm · {} compaction resets",
            value["calls_checked"].as_u64().unwrap_or(0),
            value["warm_calls_checked"].as_u64().unwrap_or(0),
            value["compactions_checked"].as_u64().unwrap_or(0)
        );
        println!(
            "  hit ratio        {:.2}% · read {} · miss {} · write {}",
            value["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_miss_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0)
        );
        if let Some(theoretical) = value["theoretical_max_hit_ratio"].as_f64() {
            println!(
                "  theoretical max  {:.2}% · append-only {} · breaks {} · epochs {}",
                theoretical * 100.0,
                value["append_only_calls"].as_u64().unwrap_or(0),
                value["append_only_breaks"].as_u64().unwrap_or(0),
                value["cache_epochs"].as_u64().unwrap_or(0)
            );
        }
        if value["clean"].as_bool().unwrap_or(false) {
            println!("  result           clean (no warning or critical findings)");
        }
        for finding in value["findings"].as_array().into_iter().flatten() {
            println!(
                "  [{:<8}] {:<32} {}",
                finding["severity"].as_str().unwrap_or("info"),
                finding["code"].as_str().unwrap_or("?"),
                finding["message"].as_str().unwrap_or("")
            );
            if let Some(evidence) = finding["evidence"].as_object()
                && !evidence.is_empty()
            {
                let rendered = evidence
                    .iter()
                    .map(|(key, value)| format!("{key}={}", compact_json(value)))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("             evidence: {rendered}");
            }
        }
        return true;
    }
    if let Some(lines) = tool_bundle_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("policy").is_some()
        && value
            .get("roles")
            .and_then(Value::as_array)
            .is_some_and(|roles| roles.iter().any(|role| role.get("prefix_hash").is_some()))
    {
        println!("cache plan  {session_id}");
        if let Some(provider) = value["provider"].as_str() {
            println!("  provider         {provider}");
        }
        if let Some(policy) = value.get("policy") {
            println!(
                "  policy           enabled {} · ttl {} · automatic {} · role overrides {}",
                policy["enabled"].as_bool().unwrap_or(true),
                policy["ttl"].as_str().unwrap_or("5m"),
                policy["automatic_caching"].as_bool().unwrap_or(true),
                policy["role_ttl"]
                    .as_object()
                    .map_or(0, serde_json::Map::len)
            );
        }
        for role in value["roles"].as_array().into_iter().flatten() {
            println!(
                "  {:<9} {} · {} breakpoints · ttl {} · tail {}",
                role["role"].as_str().unwrap_or("?"),
                role["model"].as_str().unwrap_or("?"),
                role["cache_breakpoints"].as_u64().unwrap_or(0),
                role["cache_ttl"].as_str().unwrap_or("5m"),
                role["tail_message"].as_bool().unwrap_or(false)
            );
            println!(
                "            core {}… project {}… tools {}… prefix {}…",
                short_hash(&role["core_hash"]),
                short_hash(&role["project_hash"]),
                short_hash(&role["tools_hash"]),
                short_hash(&role["prefix_hash"])
            );
            println!(
                "            sections {} core / {} project chars · {} tools · ~{} + {} tokens",
                role["core_chars"].as_u64().unwrap_or(0),
                role["project_chars"].as_u64().unwrap_or(0),
                role["tool_count"].as_u64().unwrap_or(0),
                role["system_tokens_estimate"].as_u64().unwrap_or(0),
                role["tool_tokens_estimate"].as_u64().unwrap_or(0)
            );
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("warmed").and_then(Value::as_bool).is_some()
        && value.get("source").and_then(Value::as_str).is_some()
    {
        println!(
            "cache warm  {session_id}  [{}]  {}",
            value["source"].as_str().unwrap_or("manual"),
            if value["warmed"].as_bool().unwrap_or(false) {
                "warmed"
            } else {
                "not warmed"
            }
        );
        println!(
            "  provider         {} · {}",
            value["provider"].as_str().unwrap_or("?"),
            value["model"].as_str().unwrap_or("?")
        );
        println!(
            "  breakpoints      {} · ttl {} · automatic {}",
            value["cache_breakpoints"].as_u64().unwrap_or(0),
            value["cache_ttl"].as_str().unwrap_or("?"),
            value["automatic"].as_bool().unwrap_or(false)
        );
        println!(
            "  tokens           input {} · cache read {} · cache write {} · output {}",
            value["input_tokens"].as_u64().unwrap_or(0),
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_write_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0)
        );
        println!(
            "  cost             ${:.6}{}",
            value["cost_usd"].as_f64().unwrap_or(0.0),
            if value["estimated"].as_bool().unwrap_or(false) {
                " (estimated usage)"
            } else {
                ""
            }
        );
        if let Some(message) = value["message"]
            .as_str()
            .filter(|message| !message.is_empty())
        {
            println!("  message          {message}");
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value
            .get("cache_hit_ratio")
            .and_then(Value::as_f64)
            .is_some()
        && value.get("input_tokens").is_some()
    {
        let ratio = value["cache_hit_ratio"].as_f64().unwrap_or(0.0);
        println!("cache  {session_id}");
        println!(
            "  hit ratio        {:.2}%  (cache read / ledger input buckets)",
            ratio * 100.0
        );
        println!(
            "  tokens           input {} · cache read {} · cache miss {} · cache write {} · output {}",
            value["input_tokens"].as_u64().unwrap_or(0),
            value["cache_read_tokens"].as_u64().unwrap_or(0),
            value["cache_miss_tokens"].as_u64().unwrap_or_else(|| {
                value["input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(value["cache_read_tokens"].as_u64().unwrap_or(0))
            }),
            value["cache_write_tokens"].as_u64().unwrap_or(0),
            value["output_tokens"].as_u64().unwrap_or(0)
        );
        if let Some(theoretical) = value["theoretical_max_hit_ratio"].as_f64() {
            println!(
                "  theoretical max  {:.2}%  (previous prompt / current input; append-only headroom)",
                theoretical * 100.0
            );
        }
        if let (Some(p50), Some(p95)) = (
            value["hit_ratio_p50"].as_f64(),
            value["hit_ratio_p95"].as_f64(),
        ) {
            println!(
                "  percentiles      p50 {:.2}% · p95 {:.2}%",
                p50 * 100.0,
                p95 * 100.0
            );
        }
        println!(
            "  continuity       append-only {} · breaks {} · last {}",
            value["append_only_calls"].as_u64().unwrap_or(0),
            value["append_only_breaks"].as_u64().unwrap_or(0),
            match value["last_append_only"].as_bool() {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        );
        if value["cache_epoch"].as_str().is_some() {
            println!(
                "  cache epoch      {} ({})",
                short_hash(&value["cache_epoch"]),
                value["cache_epoch_source"].as_str().unwrap_or("unknown")
            );
        }
        if let Some(reasons) = value["top_miss_reasons"].as_array()
            && !reasons.is_empty()
        {
            let rendered = reasons
                .iter()
                .map(|reason| {
                    format!(
                        "{}×{}",
                        reason["key"].as_str().unwrap_or("?"),
                        reason["calls"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            println!("  top misses       {rendered}");
        }
        if let Some(roles) = value["per_role"].as_array()
            && !roles.is_empty()
        {
            let rendered = roles
                .iter()
                .map(|role| {
                    format!(
                        "{} {:.1}% ({} miss, ${:.4})",
                        role["role"].as_str().unwrap_or("?"),
                        role["hit_ratio"].as_f64().unwrap_or(0.0) * 100.0,
                        role["cache_miss_tokens"].as_u64().unwrap_or(0),
                        role["cost_usd"].as_f64().unwrap_or(0.0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            println!("  per role         {rendered}");
        }
        println!(
            "  cross-session    {} session(s) · {} hit token(s) observed (bounded scan, not a guarantee)",
            value["cross_session_reuse_sessions"].as_u64().unwrap_or(0),
            value["cross_session_hit_tokens"].as_u64().unwrap_or(0)
        );
        println!(
            "  lost to compact  ${:.6} (estimated cache-read savings invalidated by compaction)",
            value["compaction_lost_cache_usd"].as_f64().unwrap_or(0.0)
        );
        println!(
            "  savings          ${:.6} (estimated from configured prices; negative means cache writes cost more than reads saved)",
            value["estimated_savings_usd"].as_f64().unwrap_or(0.0)
        );
        println!(
            "  calls            {} cache-observed provider calls",
            value["cache_calls"].as_u64().unwrap_or(0)
        );
        println!(
            "  plan             {} breakpoints · ttl {}",
            value["last_breakpoints"].as_u64().unwrap_or(0),
            value["last_cache_ttl"].as_str().unwrap_or("unknown")
        );
        println!(
            "  last miss        {}",
            value["last_miss_reason"]
                .as_str()
                .unwrap_or("none recorded")
        );
        match value["preamble_tokens_estimate"].as_u64() {
            Some(tokens) => println!("  preamble         ~{tokens} tokens (estimate)"),
            None => println!("  preamble         unknown"),
        }
        match value["prompt_prefix"].as_str() {
            Some(prefix) => println!("  prompt prefix    {prefix}"),
            None => println!("  prompt prefix    unknown"),
        }
        match value["last_call_at"].as_str() {
            Some(last_call) => println!("  last call        {last_call}"),
            None => println!("  last call        never"),
        }
        if value.get("append_only_calls").is_some() {
            println!(
                "  append-only      {} ({} append · {} break{})",
                value["last_append_only"].as_bool().unwrap_or(false),
                value["append_only_calls"].as_u64().unwrap_or(0),
                value["non_append_only_calls"].as_u64().unwrap_or(0),
                value["last_tail_memory_mode"]
                    .as_str()
                    .map(|mode| format!(" · tail {mode}"))
                    .unwrap_or_default()
            );
            println!(
                "  message chain    {} · previous {} · {} messages",
                short_hash(&value["last_message_prefix_hash"]),
                short_hash(&value["last_previous_message_prefix_hash"]),
                value["last_message_count"].as_u64().unwrap_or(0)
            );
            if let Some(capability) = value["last_tail_system_capability"].as_str() {
                println!(
                    "  provider tail    system {} · capability {capability}",
                    value["provider_tail_system"].as_bool().unwrap_or(false)
                );
            }
            let breaks = value["recent_breaks"].as_array();
            if let Some(breaks) = breaks.filter(|breaks| !breaks.is_empty()) {
                println!("  breaks           {} recent", breaks.len());
                for entry in breaks.iter().take(5) {
                    let index = entry["first_differing_index"]
                        .as_u64()
                        .map(|index| format!(" at #{index}"))
                        .unwrap_or_default();
                    let role = entry["first_differing_role"]
                        .as_str()
                        .map(|role| format!(" · {role}"))
                        .unwrap_or_default();
                    println!(
                        "    {:<20} {}{}{}",
                        entry["reason"].as_str().unwrap_or("unknown"),
                        entry["at"].as_str().unwrap_or(""),
                        index,
                        role
                    );
                }
            }
        }
        if let Some(policy) = value.get("policy").filter(|policy| policy.is_object()) {
            println!(
                "  policy           enabled {} · ttl {} · automatic {} · warm-on-resume {} · compact-aware {}",
                policy["enabled"].as_bool().unwrap_or(true),
                policy["ttl"].as_str().unwrap_or("5m"),
                policy["automatic_caching"].as_bool().unwrap_or(true),
                policy["warm_on_resume"].as_bool().unwrap_or(false),
                policy["compact_aware"].as_bool().unwrap_or(true)
            );
        }
        return true;
    }
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && value.get("total_breaks").is_some()
        && value.get("breaks").is_some()
    {
        println!(
            "cache breaks  {session_id}  ({} append · {} break · {} recorded{})",
            value["append_only_calls"].as_u64().unwrap_or(0),
            value["non_append_only_calls"].as_u64().unwrap_or(0),
            value["total_breaks"].as_u64().unwrap_or(0),
            if value["truncated"].as_bool().unwrap_or(false) {
                " · truncated"
            } else {
                ""
            }
        );
        for entry in value["breaks"].as_array().into_iter().flatten() {
            let index = entry["first_differing_index"]
                .as_u64()
                .map(|index| format!(" at #{index}"))
                .unwrap_or_default();
            let role = entry["first_differing_role"]
                .as_str()
                .map(|role| format!(" · {role}"))
                .unwrap_or_default();
            let tool = entry["first_differing_tool_call_id"]
                .as_str()
                .map(|tool| format!(" · {tool}"))
                .unwrap_or_default();
            println!(
                "  {:<20} {} · {}→{} msgs{}{}{}",
                entry["reason"].as_str().unwrap_or("unknown"),
                entry["at"].as_str().unwrap_or(""),
                entry["previous_message_count"].as_u64().unwrap_or(0),
                entry["message_count"].as_u64().unwrap_or(0),
                index,
                role,
                tool
            );
        }
        return true;
    }
    false
}

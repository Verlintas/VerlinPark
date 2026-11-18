//! Human renderers for instruction and tool-bundle inspection.

use super::support::*;
use super::*;

pub(crate) fn instruction_lines(value: &Value) -> Option<Vec<String>> {
    value.get("prompt_layout")?;
    let states = value.get("states")?.as_array()?;
    let session = value
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let layout = value
        .get("prompt_layout")
        .and_then(Value::as_u64)
        .unwrap_or(2);
    let prefix = value
        .get("latest_prefix_hash")
        .and_then(Value::as_str)
        .map(|hash| hash.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "none".to_string());
    let identity = value
        .get("latest_identity_id")
        .and_then(Value::as_str)
        .filter(|identity| !identity.is_empty())
        .map(|identity| {
            let generation = value
                .get("latest_identity_generation")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            format!(
                " · identity {}… gen {generation}",
                identity.chars().take(12).collect::<String>()
            )
        })
        .unwrap_or_default();
    let mut lines = vec![format!(
        "instructions · {session} · layout v{layout} · prefix {prefix}…{identity} · {} state(s)",
        states.len()
    )];
    if value
        .get("last_rebuilt")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        lines.push(format!(
            "rebuilt diagnostics in window: {}",
            value
                .get("last_rebuilt")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ));
    }
    for state in states {
        let state_id = state
            .get("state_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let role = state
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("task-scoped");
        lines.push(format!("  {state_id} · {role}"));
        let Some(sections) = state.get("sections").and_then(Value::as_array) else {
            continue;
        };
        for section in sections {
            let ordinal = section.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
            let tail = section
                .get("tail")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let key = section.get("key").and_then(Value::as_str).unwrap_or("?");
            let hash = section
                .get("hash")
                .and_then(Value::as_str)
                .map(|hash| hash.chars().take(12).collect::<String>())
                .unwrap_or_default();
            let chars = section.get("chars").and_then(Value::as_u64).unwrap_or(0);
            let available = section
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            lines.push(format!(
                "    {ordinal:>3} {:<4} {key:<18} {chars:>7} chars  {hash}…{}",
                if tail { "tail" } else { "head" },
                if available { "" } else { "  MISSING" }
            ));
        }
    }
    Some(lines)
}

pub(crate) fn tool_bundle_lines(value: &Value) -> Option<Vec<String>> {
    if let Some(entries) = value.get("entries").and_then(Value::as_array)
        && let Some(session_id) = value.get("session_id").and_then(Value::as_str)
        && entries.iter().all(|entry| entry.get("bundle").is_some())
    {
        let mut lines = vec![format!("tool bundle history  {session_id}")];
        if entries.is_empty() {
            lines.push("  no persisted bundle observations".to_string());
        }
        for entry in entries {
            lines.push(format!(
                "  seq {:<6} {:<10} {:<13} {:>3} tools · {} mcp · {} · {}{}",
                entry["seq"].as_u64().unwrap_or(0),
                entry["role"].as_str().unwrap_or("?"),
                entry["bundle"].as_str().unwrap_or("?"),
                entry["tool_count"].as_u64().unwrap_or(0),
                entry["mcp_tool_count"].as_u64().unwrap_or(0),
                entry["created_at"].as_str().unwrap_or(""),
                short_hash(&entry["bundle_hash"]),
                if entry["fallback"].as_bool().unwrap_or(false) {
                    " fallback"
                } else {
                    ""
                }
            ));
        }
        return Some(lines);
    }
    if value.get("from_bundle").is_some()
        && value.get("to_bundle").is_some()
        && value.get("added").is_some()
    {
        let mut lines = vec![format!(
            "tool bundle diff  {} → {}",
            value["from_bundle"].as_str().unwrap_or("?"),
            value["to_bundle"].as_str().unwrap_or("?")
        )];
        lines.push(format!(
            "  role {} · profile {} · task kind {}",
            value["role"].as_str().unwrap_or("?"),
            value["profile"].as_str().unwrap_or("?"),
            value["task_kind"].as_str().unwrap_or("—")
        ));
        lines.push(format!(
            "  {} → {} tools · changed {}",
            value["from_count"].as_u64().unwrap_or(0),
            value["to_count"].as_u64().unwrap_or(0),
            value["changed"].as_bool().unwrap_or(false)
        ));
        for (label, key) in [
            ("added", "added"),
            ("removed", "removed"),
            ("shared", "shared"),
        ] {
            if let Some(items) = value[key].as_array().filter(|items| !items.is_empty()) {
                lines.push(format!(
                    "  {label:<8} {}",
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    if let Some(rows) = value.get("rows").and_then(Value::as_array)
        && value.get("task_kinds").and_then(Value::as_array).is_some()
        && rows.iter().all(|row| row.get("bundle_hash").is_some())
    {
        let mut lines = vec!["tool bundle matrix".to_string()];
        lines.push(format!(
            "  {:<10} {:<13} {:<18} {:<13} {:>7} {:>8} {:>7} {}",
            "role", "profile", "task kind", "bundle", "tools", "target", "omitted", "status"
        ));
        for row in rows {
            let task_kind = row["task_kind"].as_str().unwrap_or("—");
            let target = row["target_limit"]
                .as_u64()
                .map(|limit| format!("{limit}"))
                .unwrap_or_else(|| "—".to_string());
            let status = match row["within_target"].as_bool() {
                Some(true) => "ok",
                Some(false) => "OVER",
                None => {
                    if row["fallback"].as_bool().unwrap_or(false) {
                        "fallback"
                    } else {
                        "ok"
                    }
                }
            };
            lines.push(format!(
                "  {:<10} {:<13} {:<18} {:<13} {:>7} {:>8} {:>7} {}",
                row["role"].as_str().unwrap_or("?"),
                row["profile"].as_str().unwrap_or("?"),
                task_kind,
                row["bundle"].as_str().unwrap_or("?"),
                row["builtin_count"].as_u64().unwrap_or(0),
                target,
                row["omitted_count"].as_u64().unwrap_or(0),
                status
            ));
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    if let Some(bundles) = value.get("bundles").and_then(Value::as_array)
        && bundles
            .iter()
            .all(|row| row.get("requested_count").is_some())
    {
        let mut lines = vec!["tool bundles".to_string()];
        for row in bundles {
            let target = row["target_limit"]
                .as_u64()
                .map(|limit| format!("/{limit}"))
                .unwrap_or_default();
            lines.push(format!(
                "  {:<12} requested {}{target} · {} mcp · {}/{}",
                row["bundle"].as_str().unwrap_or("?"),
                row["requested_count"].as_u64().unwrap_or(0),
                if row["allows_dynamic_mcp"].as_bool().unwrap_or(false) {
                    "appends"
                } else {
                    "no"
                },
                row["representative_role"].as_str().unwrap_or("?"),
                row["representative_profile"].as_str().unwrap_or("?")
            ));
            if let Some(summary) = row["summary"].as_str() {
                lines.push(format!("               {summary}"));
            }
        }
        for warning in value["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "  [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        return Some(lines);
    }
    let session_id = value.get("session_id")?.as_str()?;
    let roles = value.get("roles")?.as_array()?;
    if !value.get("catalog")?.as_array().is_some() {
        return None;
    }
    if !roles
        .iter()
        .any(|role| role.get("bundle").is_some() && role.get("bundle_hash").is_some())
    {
        return None;
    }
    let mut lines = vec![format!("tool bundle  {session_id}")];
    for role in roles {
        let role_name = role["role"].as_str().unwrap_or("?");
        let bundle = role["bundle"].as_str().unwrap_or("?");
        let target = role["target_limit"]
            .as_u64()
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default();
        let within = match role["within_target"].as_bool() {
            Some(true) => "ok",
            Some(false) => "OVER TARGET",
            None => "no target",
        };
        lines.push(format!(
            "  {role_name:<9} {bundle:<12} {}{target} tools · {within} · {} mcp",
            role["builtin_count"].as_u64().unwrap_or(0),
            role["mcp_count"].as_u64().unwrap_or(0)
        ));
        lines.push(format!(
            "            profile {} · task kind {} · {}",
            role["profile"].as_str().unwrap_or("?"),
            role["task_kind"].as_str().unwrap_or("—"),
            role["reason"].as_str().unwrap_or("?")
        ));
        lines.push(format!(
            "            bundle {} · tools {} · {} schema bytes · ~{} tokens",
            short_hash(&role["bundle_hash"]),
            short_hash(&role["tools_hash"]),
            role["schema_bytes"].as_u64().unwrap_or(0),
            role["estimated_tool_tokens"].as_u64().unwrap_or(0)
        ));
        if let Some(last) = role.get("last_run").filter(|last| !last.is_null()) {
            lines.push(format!(
                "            last run {} · {} tools · {} mcp · {}",
                last["bundle"].as_str().unwrap_or("?"),
                last["tool_count"].as_u64().unwrap_or(0),
                last["mcp_tool_count"].as_u64().unwrap_or(0),
                last["created_at"].as_str().unwrap_or("")
            ));
        }
        if let Some(omitted) = role["omitted"].as_array().filter(|items| !items.is_empty()) {
            lines.push(format!(
                "            omitted {}",
                omitted
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for warning in role["warnings"].as_array().into_iter().flatten() {
            lines.push(format!(
                "            [{}] {}",
                warning["code"].as_str().unwrap_or("warning"),
                warning["message"].as_str().unwrap_or("")
            ));
        }
        if let Some(categories) = role["categories"]
            .as_array()
            .filter(|categories| !categories.is_empty())
        {
            let rendered = categories
                .iter()
                .map(|category| {
                    format!(
                        "{} {}",
                        category["category"].as_str().unwrap_or("?"),
                        category["tools"].as_u64().unwrap_or(0)
                    )
                })
                .collect::<Vec<_>>()
                .join(" · ");
            lines.push(format!("            categories {rendered}"));
        }
        if let Some(tools) = role["tools"].as_array().filter(|tools| !tools.is_empty()) {
            let mut line = String::from("            tools ");
            let mut first = true;
            for tool in tools {
                if !first {
                    line.push_str(", ");
                }
                first = false;
                line.push_str(tool["name"].as_str().unwrap_or("?"));
            }
            lines.push(line);
        }
    }
    if let Some(catalog) = value.get("catalog").and_then(Value::as_array) {
        lines.push("  catalog".to_string());
        for row in catalog {
            let target = row["target_limit"]
                .as_u64()
                .map(|limit| format!("/{limit}"))
                .unwrap_or_default();
            lines.push(format!(
                "    {:<12} requested {}{target} · {} mcp",
                row["bundle"].as_str().unwrap_or("?"),
                row["requested_count"].as_u64().unwrap_or(0),
                if row["allows_dynamic_mcp"].as_bool().unwrap_or(false) {
                    "appends"
                } else {
                    "no"
                }
            ));
        }
    }
    Some(lines)
}

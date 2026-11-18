//! Human renderer chunk: programs.

use crate::render::support::*;
use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(programs) = value.get("programs").and_then(Value::as_array) {
        if programs.is_empty() {
            println!("No programs.");
            return true;
        }
        for entry in programs {
            let program = entry.get("program").unwrap_or(entry);
            let id = program["id"].as_str().unwrap_or("?");
            let status = program["status"].as_str().unwrap_or("?");
            let goal = program["goal"].as_str().unwrap_or("");
            let counts = &entry["counts"];
            println!(
                "{id}  {status:<10}  {goal}  milestones {}/{} done · {} running · {} blocked",
                counts["completed"].as_u64().unwrap_or(0),
                counts["pending"].as_u64().unwrap_or(0)
                    + counts["ready"].as_u64().unwrap_or(0)
                    + counts["running"].as_u64().unwrap_or(0)
                    + counts["verifying"].as_u64().unwrap_or(0)
                    + counts["completed"].as_u64().unwrap_or(0)
                    + counts["failed"].as_u64().unwrap_or(0)
                    + counts["blocked"].as_u64().unwrap_or(0)
                    + counts["cancelled"].as_u64().unwrap_or(0),
                counts["running"].as_u64().unwrap_or(0) + counts["verifying"].as_u64().unwrap_or(0),
                counts["blocked"].as_u64().unwrap_or(0),
            );
            print_program_line(program);
        }
        return true;
    }
    if let Some(program) = value
        .get("program")
        .filter(|program| program["id"].is_string())
    {
        print_program_line(program);
        if let Some(milestones) = value.get("milestones").and_then(Value::as_array) {
            for milestone in milestones {
                let key = milestone["key"].as_str().unwrap_or("?");
                let id = milestone["id"].as_str().unwrap_or("?");
                let status = milestone["status"].as_str().unwrap_or("?");
                let title = milestone["title"].as_str().unwrap_or("");
                println!(
                    "  {key:<8} {status:<10} {title}  attempts {}/{}{}",
                    milestone["attempts"].as_u64().unwrap_or(0),
                    milestone["max_attempts"].as_u64().unwrap_or(0),
                    milestone["session_id"]
                        .as_str()
                        .map(|session| format!("  session {session}"))
                        .unwrap_or_default()
                );
                if let Some(deps) = milestone["deps"].as_array().filter(|deps| !deps.is_empty()) {
                    println!(
                        "    deps {}",
                        deps.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(error) = milestone["last_error"].as_str() {
                    println!("    {error}");
                }
                if let Some(next) = milestone["next_attempt_at"].as_str() {
                    println!("    retry after {next}");
                }
                let _ = id;
            }
        }
        if let Some(usage) = value.get("usage") {
            println!(
                "  usage {} tokens / ${:.4}",
                usage["token_spent"].as_u64().unwrap_or(0),
                usage["cost_spent_usd"].as_f64().unwrap_or(0.0)
            );
        }
        return true;
    }
    false
}

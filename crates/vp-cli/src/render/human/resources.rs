//! Human-readable Run Contract/resource/readiness views.

use super::*;

pub(crate) fn print(value: &Value) -> bool {
    if let Some(dimensions) = value.get("dimensions").and_then(Value::as_array) {
        print_resources(value, dimensions);
        return true;
    }
    if value.get("shippable").is_some() && value.get("level").is_some() {
        print_readiness(value);
        return true;
    }
    if value.get("stop_policy").is_some() && value.get("hard_caps").is_some() {
        print_contract(value);
        return true;
    }
    false
}

fn print_resources(value: &Value, dimensions: &[Value]) {
    println!(
        "resources: profile={} pricing={} bottleneck={}",
        value
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("pricing_source")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("bottleneck")
            .and_then(Value::as_str)
            .unwrap_or("none")
    );
    if let Some(readiness) = value.get("readiness") {
        println!(
            "readiness: {} shippable={} mergeable={}",
            readiness
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            readiness
                .get("shippable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            readiness
                .get("mergeable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
    }
    for dimension in dimensions {
        let key = dimension.get("key").and_then(Value::as_str).unwrap_or("?");
        let used = dimension.get("used").and_then(Value::as_f64).unwrap_or(0.0);
        let unit = dimension.get("unit").and_then(Value::as_str).unwrap_or("");
        let state = dimension
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let hard = dimension
            .get("hard_cap")
            .and_then(Value::as_f64)
            .map(|cap| format!(" / {cap:.0}"))
            .unwrap_or_default();
        let remaining = dimension
            .get("remaining")
            .and_then(Value::as_f64)
            .map(|value| format!(" remaining={value:.0}"))
            .unwrap_or_default();
        println!("  {key}: {used:.2}{hard} {unit} [{state}]{remaining}");
        if let Some(note) = dimension.get("note").and_then(Value::as_str)
            && !note.is_empty()
        {
            println!("    {note}");
        }
    }
    if let Some(attention) = value.get("attention") {
        println!(
            "  attention: pending={} approved={} denied={} interrupts={}",
            attention
                .get("pending")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            attention
                .get("approved")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            attention.get("denied").and_then(Value::as_u64).unwrap_or(0),
            attention
                .get("interrupts")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        );
    }
    if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
        for warning in warnings {
            if let Some(warning) = warning.as_str() {
                println!("  warning: {warning}");
            }
        }
    }
}

fn print_readiness(value: &Value) {
    println!(
        "readiness: level={} shippable={} mergeable={}",
        value
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("shippable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        value
            .get("mergeable")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    for key in ["latest_commit", "verifier", "merge"] {
        if let Some(value) = value.get(key).and_then(Value::as_str) {
            println!("  {key}: {value}");
        }
    }
    if let Some(paths) = value.get("changed_paths").and_then(Value::as_array)
        && !paths.is_empty()
    {
        println!("  changed_paths:");
        for path in paths.iter().filter_map(Value::as_str) {
            println!("    {path}");
        }
    }
    if let Some(work) = value.get("remaining_work").and_then(Value::as_array)
        && !work.is_empty()
    {
        println!("  remaining_work:");
        for item in work.iter().filter_map(Value::as_str) {
            println!("    {item}");
        }
    }
    if let Some(blockers) = value.get("blockers").and_then(Value::as_array)
        && !blockers.is_empty()
    {
        println!("  blockers:");
        for blocker in blockers.iter().filter_map(Value::as_str) {
            println!("    {blocker}");
        }
    }
}

fn print_contract(value: &Value) {
    println!(
        "run-contract: profile={} source={} risk={} autonomy={}",
        value
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("risk_ceiling")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        value
            .get("autonomy")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    if let Some(caps) = value.get("hard_caps") {
        println!(
            "  hard_caps: token={} cost_usd={} wall_clock_secs={}",
            caps.get("token").map(Value::to_string).unwrap_or_default(),
            caps.get("cost_usd")
                .map(Value::to_string)
                .unwrap_or_default(),
            caps.get("wall_clock_secs")
                .map(Value::to_string)
                .unwrap_or_default()
        );
    }
    println!(
        "  completion_reserve_ratio={} attention_cap={}",
        value
            .get("completion_reserve_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        value
            .get("attention_cap")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    );
    if let Some(acceptance) = value.get("acceptance").and_then(Value::as_array)
        && !acceptance.is_empty()
    {
        println!("  acceptance:");
        for item in acceptance.iter().filter_map(Value::as_str) {
            println!("    {item}");
        }
    }
    if let Some(warnings) = value.get("warnings").and_then(Value::as_array) {
        for warning in warnings.iter().filter_map(Value::as_str) {
            println!("  warning: {warning}");
        }
    }
}

//! Ordered human renderer dispatch.

mod briefs;
mod maintenance;
mod mcp;
mod memory;
mod orchestration;
mod programs;
mod projects;
mod sessions;
mod sources;
mod status;

use super::*;
use crate::cli::Command;

/// Command-family hint for result shapes that are ambiguous on their own
/// (for example the bare `{"removed": N}` lease-sweep and mailbox-GC results).
pub(crate) fn human_hint(command: &Command) -> Option<&'static str> {
    match command {
        Command::LeasesSweep => Some("runtime.leases.sweep"),
        Command::McpRemove { .. } => Some("mcp.server.remove"),
        Command::AcpRemove { .. } => Some("acp.agent.remove"),
        Command::HostRemove { .. } => Some("host.remove"),
        Command::ModelPresetDelete { .. } => Some("model.preset.delete"),
        Command::MailboxGc { .. } => Some("mailbox.gc"),
        Command::Gc { .. } => Some("artifact.gc"),
        Command::MemoryDelete { .. } => Some("memory.delete"),
        Command::MemoryPrune { .. } => Some("memory.prune"),
        _ => None,
    }
}

/// Render a removal result only for the command family that produced it. This
/// prevents the lease-sweep wording from shadowing MCP/ACP/host/model-preset
/// deletes and mailbox/artifact GC.
pub(crate) fn removal_line(hint: Option<&str>, value: &Value) -> Option<String> {
    match hint? {
        "runtime.leases.sweep" => value
            .get("removed")
            .and_then(Value::as_u64)
            .map(|removed| format!("Removed {removed} expired runtime lease(s).")),
        "mcp.server.remove" => value
            .get("removed")
            .and_then(Value::as_bool)
            .map(|removed| format!("MCP server removed={removed}")),
        "acp.agent.remove" => value
            .get("removed")
            .and_then(Value::as_bool)
            .map(|removed| format!("ACP agent removed={removed}")),
        "host.remove" => value
            .get("removed")
            .and_then(Value::as_bool)
            .map(|removed| format!("execution host removed={removed}")),
        "model.preset.delete" => value
            .get("removed")
            .and_then(Value::as_bool)
            .map(|removed| format!("model preset removed={removed}")),
        "mailbox.gc" => value
            .get("removed")
            .and_then(Value::as_u64)
            .map(|removed| format!("mailbox GC removed {removed} message(s)")),
        "artifact.gc" => {
            let removed = value.get("removed").and_then(Value::as_u64)?;
            let bytes = value
                .get("bytes_freed")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Some(format!(
                "artifact GC removed {removed} artifact(s), freed {bytes} bytes"
            ))
        }
        "memory.delete" => {
            let memory = value.get("memory_id").and_then(Value::as_str)?;
            let removed = value
                .get("removed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(format!("memory {memory} removed={removed}"))
        }
        "memory.prune" => {
            let candidates = value.get("candidates").and_then(Value::as_u64)?;
            let removed = value.get("removed").and_then(Value::as_u64).unwrap_or(0);
            let dry_run = value
                .get("dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(format!(
                "memory prune: candidates={candidates} removed={removed} dry_run={dry_run}"
            ))
        }
        _ => None,
    }
}

pub(crate) fn print_human(value: &Value) {
    print_human_for(None, value);
}

pub(crate) fn print_human_for(hint: Option<&str>, value: &Value) {
    if let Some(line) = removal_line(hint, value) {
        println!("{line}");
        return;
    }
    if briefs::print(value) {
        return;
    }
    if programs::print(value) {
        return;
    }
    if sessions::print(value) {
        return;
    }
    if orchestration::print(value) {
        return;
    }
    if sources::print(value) {
        return;
    }
    if projects::print(value) {
        return;
    }
    if mcp::print(value) {
        return;
    }
    if memory::print(value) {
        return;
    }
    if maintenance::print(value) {
        return;
    }
    if status::print(value) {
        return;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

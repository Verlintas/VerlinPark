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

pub(crate) fn print_human(value: &Value) {
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

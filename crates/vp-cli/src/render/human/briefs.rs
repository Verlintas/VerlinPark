//! Human renderer chunk: briefs.

use crate::render::activity::*;
use crate::render::effects::*;
use crate::render::forges::*;
use crate::render::identity::*;
use crate::render::instructions::*;
use crate::render::runtime::*;
use serde_json::Value;

pub(super) fn print(value: &Value) -> bool {
    if let Some(lines) = agent_org_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = agent_detail_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = agent_employment_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = workspace_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = coordination_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = agent_memory_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = agent_identity_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = team_template_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = diagnosis_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = runtime_pin_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = controller_replay_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = gate_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = effect_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = instruction_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = forge_preflight_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = forge_status_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = pull_request_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = [
        archive_list_lines(value),
        archive_extract_lines(value),
        media_info_lines(value),
        media_transcode_lines(value),
    ]
    .into_iter()
    .flatten()
    .next()
    {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = test_cache_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = process_console_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = governor_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = remote_bootstrap_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = remote_test_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = remote_integration_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = lsp_status_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = scaling_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = tool_activity_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    if let Some(lines) = context_checkpoint_lines(value) {
        for line in lines {
            println!("{line}");
        }
        return true;
    }
    false
}
